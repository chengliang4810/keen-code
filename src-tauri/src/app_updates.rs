//! 应用版本信息、后台预下载与签名更新安装。

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::app_settings::AppUpdateDownloadSource;

/// 一次发布构建写入的对外版本标签；本地开发构建没有该变量。
const RELEASE_TAG: Option<&str> = option_env!("KEENCODE_RELEASE_TAG");
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(20);
/// 安装包可能需要经过 GitHub 跳转和代理下载，不能沿用清单检查的 20 秒超时。
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CHINA_MIRROR_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const GHFAST_PREFIX: &str = "https://ghfast.top/";
const UPDATE_PROGRESS_EVENT: &str = "app://update-status";
const UPDATE_PROGRESS_EMIT_BYTES: u64 = 256 * 1024;

/// Windows updater 在解包成功后会调用同步 hook，随后启动安装器并直接
/// `process::exit`。在独立线程中等待异步清理，避免 Tokio runtime 内嵌套 block_on。
#[cfg(windows)]
fn prepare_windows_update_exit(app: AppHandle) {
    let cleanup_app = app.clone();
    let cleanup = std::thread::spawn(move || {
        tauri::async_runtime::block_on(crate::app_exit::prepare_for_exit(&cleanup_app))
    })
    .join();
    match cleanup {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::error!(%error, "Windows 更新退出清理失败，安装器仍将继续"),
        Err(_) => tracing::error!("Windows 更新退出清理线程异常，安装器仍将继续"),
    }
    app.cleanup_before_exit();
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum AppUpdateDownloadState {
    #[default]
    Idle,
    Downloading,
    Verifying,
    Ready,
    Installing,
    Failed,
}

#[derive(Clone)]
struct VerifiedUpdateCache {
    path: PathBuf,
    /// 下载完成并通过 minisign 后计算；安装前再次校验磁盘缓存未被改写。
    sha256: [u8; 32],
}

#[derive(Default)]
struct PendingUpdateState {
    checked: bool,
    update: Option<Update>,
    download_state: AppUpdateDownloadState,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    download_source: Option<AppUpdateDownloadSource>,
    download_error: Option<String>,
    cache: Option<VerifiedUpdateCache>,
    operation_id: u64,
}

/// 尚未安装的签名更新及后台下载状态。
#[derive(Clone, Default)]
pub struct PendingUpdate(Arc<Mutex<PendingUpdateState>>);

/// 前端展示的当前版本与最近一次检查、下载结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateStatus {
    current_version: String,
    current_release: String,
    checked: bool,
    available: bool,
    latest_version: Option<String>,
    latest_release: Option<String>,
    notes: Option<String>,
    published_at: Option<String>,
    download_state: AppUpdateDownloadState,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    download_source: Option<AppUpdateDownloadSource>,
    download_error: Option<String>,
}

impl AppUpdateStatus {
    fn current(app: &AppHandle) -> Self {
        let current_version = app.package_info().version.to_string();
        Self {
            current_release: current_release(&current_version),
            current_version,
            checked: false,
            available: false,
            latest_version: None,
            latest_release: None,
            notes: None,
            published_at: None,
            download_state: AppUpdateDownloadState::Idle,
            downloaded_bytes: 0,
            total_bytes: None,
            download_source: None,
            download_error: None,
        }
    }

    fn from_pending(app: &AppHandle, pending: &PendingUpdateState) -> Self {
        let mut status = Self::current(app);
        status.checked = pending.checked;
        status.download_state = pending.download_state;
        status.downloaded_bytes = pending.downloaded_bytes;
        status.total_bytes = pending.total_bytes;
        status.download_source = pending.download_source;
        status.download_error = pending.download_error.clone();

        if let Some(update) = pending.update.as_ref() {
            status.available = true;
            status.latest_version = Some(update.version.clone());
            status.latest_release = Some(release_from_manifest(
                &update.raw_json,
                &update.download_url,
                &update.version,
            ));
            status.notes = update.body.clone().filter(|body| !body.trim().is_empty());
            status.published_at = update.date.map(|date| date.to_string());
        }
        status
    }
}

fn current_release(current_version: &str) -> String {
    RELEASE_TAG
        .filter(|tag| !tag.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("v{current_version}-dev"))
}

/// 从发布清单读取对外标签；旧式下载地址仍可作为诊断回退。
fn release_from_manifest(
    manifest: &serde_json::Value,
    download_url: &url::Url,
    version: &str,
) -> String {
    if let Some(release) = manifest
        .get("release")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|release| !release.is_empty())
    {
        return release.to_owned();
    }

    let segments: Vec<_> = download_url
        .path_segments()
        .map(Iterator::collect)
        .unwrap_or_default();
    segments
        .windows(2)
        .find_map(|pair| (pair[0] == "download").then(|| pair[1].to_owned()))
        .filter(|tag| !tag.is_empty())
        .unwrap_or_else(|| format!("v{version}"))
}

fn pending_lock(
    pending: &PendingUpdate,
) -> Result<std::sync::MutexGuard<'_, PendingUpdateState>, String> {
    pending
        .0
        .lock()
        .map_err(|_| "更新状态暂时不可用，请重试。".to_owned())
}

fn pending_status(app: &AppHandle, pending: &PendingUpdate) -> Result<AppUpdateStatus, String> {
    let state = pending_lock(pending)?;
    Ok(AppUpdateStatus::from_pending(app, &state))
}

fn emit_pending_status(app: &AppHandle, pending: &PendingUpdate) {
    if let Ok(status) = pending_status(app, pending) {
        let _ = app.emit(UPDATE_PROGRESS_EVENT, status);
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn mark_download_failed(
    app: &AppHandle,
    pending: &PendingUpdate,
    operation_id: u64,
    message: String,
) {
    let changed = pending_lock(pending)
        .map(|mut state| {
            if state.operation_id != operation_id {
                return false;
            }
            state.download_state = AppUpdateDownloadState::Failed;
            state.download_error = Some(message);
            state.cache = None;
            true
        })
        .unwrap_or(false);
    if changed {
        emit_pending_status(app, pending);
    }
}

#[derive(Clone)]
struct UpdateDownloadAttempt {
    source: AppUpdateDownloadSource,
    url: url::Url,
    timeout: Duration,
}

fn china_mirror_url(download_url: &url::Url) -> Result<url::Url, String> {
    if download_url.scheme() != "https" || download_url.host_str() != Some("github.com") {
        return Err("国内加速仅支持 GitHub Releases 下载地址。".to_owned());
    }
    url::Url::parse(&format!("{GHFAST_PREFIX}{download_url}"))
        .map_err(|error| format!("国内加速下载地址无效：{error}"))
}

fn download_attempts(
    source: AppUpdateDownloadSource,
    download_url: &url::Url,
) -> Result<Vec<UpdateDownloadAttempt>, String> {
    let github = UpdateDownloadAttempt {
        source: AppUpdateDownloadSource::Github,
        url: download_url.clone(),
        timeout: UPDATE_DOWNLOAD_TIMEOUT,
    };
    match source {
        AppUpdateDownloadSource::Auto => Ok(vec![
            UpdateDownloadAttempt {
                source: AppUpdateDownloadSource::ChinaMirror,
                url: china_mirror_url(download_url)?,
                timeout: CHINA_MIRROR_DOWNLOAD_TIMEOUT,
            },
            github,
        ]),
        AppUpdateDownloadSource::Github => Ok(vec![github]),
        AppUpdateDownloadSource::ChinaMirror => Ok(vec![UpdateDownloadAttempt {
            source: AppUpdateDownloadSource::ChinaMirror,
            url: china_mirror_url(download_url)?,
            timeout: CHINA_MIRROR_DOWNLOAD_TIMEOUT,
        }]),
    }
}

fn begin_update_download(
    app: AppHandle,
    pending: PendingUpdate,
    update: Update,
) -> Result<AppUpdateStatus, String> {
    let source_preference = crate::app_settings::get(&app)
        .map_err(|error| format!("无法读取更新下载源设置：{error}"))?
        .app_update_download_source;
    let attempts = download_attempts(source_preference, &update.download_url)?;
    let cache_path = app
        .path()
        .app_cache_dir()
        .map_err(|error| format!("无法创建更新缓存目录：{error}"))?
        .join("updates")
        .join("pending-update.bin");

    let operation_id = {
        let mut state = pending_lock(&pending)?;
        let same_update = state
            .update
            .as_ref()
            .is_some_and(|current| current.version == update.version);
        if same_update
            && matches!(
                state.download_state,
                AppUpdateDownloadState::Downloading
                    | AppUpdateDownloadState::Verifying
                    | AppUpdateDownloadState::Ready
                    | AppUpdateDownloadState::Installing
            )
        {
            return Ok(AppUpdateStatus::from_pending(&app, &state));
        }

        state.operation_id = state.operation_id.wrapping_add(1);
        state.checked = true;
        state.update = Some(update.clone());
        state.download_state = AppUpdateDownloadState::Downloading;
        state.downloaded_bytes = 0;
        state.total_bytes = None;
        state.download_source = None;
        state.download_error = None;
        state.cache = None;
        state.operation_id
    };
    emit_pending_status(&app, &pending);
    let initial_status = pending_status(&app, &pending)?;
    let download_app = app.clone();
    let download_pending = pending.clone();

    tauri::async_runtime::spawn(async move {
        let app = download_app;
        let pending = download_pending;
        let mut failures = Vec::new();
        let mut downloaded = None;
        for attempt in attempts {
            let changed = pending_lock(&pending)
                .map(|mut state| {
                    if state.operation_id != operation_id {
                        return false;
                    }
                    state.download_state = AppUpdateDownloadState::Downloading;
                    state.downloaded_bytes = 0;
                    state.total_bytes = None;
                    state.download_source = Some(attempt.source);
                    state.download_error = None;
                    true
                })
                .unwrap_or(false);
            if !changed {
                return;
            }
            emit_pending_status(&app, &pending);

            let mut attempt_update = update.clone();
            attempt_update.download_url = attempt.url;
            attempt_update.timeout = Some(attempt.timeout);
            let progress_app = app.clone();
            let progress_pending = pending.clone();
            let finish_app = app.clone();
            let finish_pending = pending.clone();
            let mut bytes_since_emit = 0_u64;
            let result = attempt_update
                .download(
                    move |chunk_size, total_bytes| {
                        bytes_since_emit = bytes_since_emit.saturating_add(chunk_size as u64);
                        let should_emit = pending_lock(&progress_pending)
                            .map(|mut state| {
                                if state.operation_id != operation_id {
                                    return false;
                                }
                                state.downloaded_bytes =
                                    state.downloaded_bytes.saturating_add(chunk_size as u64);
                                if total_bytes.is_some() {
                                    state.total_bytes = total_bytes;
                                }
                                let finished = total_bytes
                                    .is_some_and(|total| state.downloaded_bytes >= total);
                                bytes_since_emit >= UPDATE_PROGRESS_EMIT_BYTES || finished
                            })
                            .unwrap_or(false);
                        if should_emit {
                            bytes_since_emit = 0;
                            emit_pending_status(&progress_app, &progress_pending);
                        }
                    },
                    move || {
                        let changed = pending_lock(&finish_pending)
                            .map(|mut state| {
                                if state.operation_id != operation_id {
                                    return false;
                                }
                                state.download_state = AppUpdateDownloadState::Verifying;
                                true
                            })
                            .unwrap_or(false);
                        if changed {
                            emit_pending_status(&finish_app, &finish_pending);
                        }
                    },
                )
                .await;
            match result {
                Ok(bytes) => {
                    downloaded = Some(bytes);
                    break;
                }
                Err(error) => failures.push(format!(
                    "{}：{error}",
                    match attempt.source {
                        AppUpdateDownloadSource::ChinaMirror => "国内加速失败",
                        AppUpdateDownloadSource::Github => "GitHub 失败",
                        AppUpdateDownloadSource::Auto => unreachable!(),
                    }
                )),
            }
        }

        let Some(bytes) = downloaded else {
            mark_download_failed(&app, &pending, operation_id, failures.join("；"));
            return;
        };

        if let Some(parent) = cache_path.parent()
            && let Err(error) = tokio::fs::create_dir_all(parent).await
        {
            mark_download_failed(
                &app,
                &pending,
                operation_id,
                format!("无法创建更新缓存目录：{error}"),
            );
            return;
        }
        if let Err(error) = tokio::fs::write(&cache_path, &bytes).await {
            mark_download_failed(
                &app,
                &pending,
                operation_id,
                format!("无法保存已校验的更新：{error}"),
            );
            return;
        }

        let digest = sha256(&bytes);
        let changed = pending_lock(&pending)
            .map(|mut state| {
                if state.operation_id != operation_id {
                    return false;
                }
                state.download_state = AppUpdateDownloadState::Ready;
                state.downloaded_bytes = bytes.len() as u64;
                state.total_bytes = Some(state.total_bytes.unwrap_or(bytes.len() as u64));
                state.download_error = None;
                state.cache = Some(VerifiedUpdateCache {
                    path: cache_path,
                    sha256: digest,
                });
                true
            })
            .unwrap_or(false);
        if changed {
            emit_pending_status(&app, &pending);
        }
    });

    Ok(initial_status)
}

/// 读取当前构建版本及后台下载进度，不访问网络。
#[tauri::command]
pub fn app_update_info(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<AppUpdateStatus, String> {
    pending_status(&app, pending.inner())
}

/// 读取 GitHub Releases 的签名更新清单；发现更新后立即开始后台预下载。
#[tauri::command]
pub async fn app_update_check(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<AppUpdateStatus, String> {
    let pending = pending.inner().clone();
    let existing = {
        let state = pending_lock(&pending)?;
        match state.download_state {
            AppUpdateDownloadState::Downloading
            | AppUpdateDownloadState::Verifying
            | AppUpdateDownloadState::Ready
            | AppUpdateDownloadState::Installing => {
                return Ok(AppUpdateStatus::from_pending(&app, &state));
            }
            AppUpdateDownloadState::Failed => state.update.clone(),
            AppUpdateDownloadState::Idle => None,
        }
    };
    if let Some(update) = existing {
        return begin_update_download(app, pending, update);
    }

    let updater = app.updater_builder().timeout(UPDATE_CHECK_TIMEOUT);
    #[cfg(windows)]
    let updater = {
        let exit_app = app.clone();
        updater.on_before_exit(move || prepare_windows_update_exit(exit_app.clone()))
    };
    let update = updater
        .build()
        .map_err(|error| format!("更新服务配置无效：{error}"))?
        .check()
        .await
        .map_err(|error| format!("无法检查 GitHub Releases：{error}"))?;

    if let Some(update) = update {
        begin_update_download(app, pending, update)
    } else {
        let old_cache = {
            let mut state = pending_lock(&pending)?;
            state.operation_id = state.operation_id.wrapping_add(1);
            state.checked = true;
            state.update = None;
            state.download_state = AppUpdateDownloadState::Idle;
            state.downloaded_bytes = 0;
            state.total_bytes = None;
            state.download_source = None;
            state.download_error = None;
            state.cache.take().map(|cache| cache.path)
        };
        if let Some(path) = old_cache {
            let _ = tokio::fs::remove_file(path).await;
        }
        pending_status(&app, &pending)
    }
}

/// 安装后台已下载并验签的更新，然后重启应用。
#[tauri::command]
pub async fn app_update_install(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<(), String> {
    let pending = pending.inner().clone();
    let (operation_id, update, cache) = {
        let mut state = pending_lock(&pending)?;
        if state.download_state != AppUpdateDownloadState::Ready {
            return Err("更新尚未下载并校验完成，请稍后重试。".to_owned());
        }
        let update = state
            .update
            .clone()
            .ok_or_else(|| "没有可安装的更新，请先重新检查。".to_owned())?;
        let cache = state
            .cache
            .clone()
            .ok_or_else(|| "已下载的更新缓存不存在，请重新下载。".to_owned())?;
        state.download_state = AppUpdateDownloadState::Installing;
        state.download_error = None;
        (state.operation_id, update, cache)
    };
    emit_pending_status(&app, &pending);

    let bytes = match tokio::fs::read(&cache.path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = tokio::fs::remove_file(&cache.path).await;
            mark_download_failed(
                &app,
                &pending,
                operation_id,
                format!("无法读取已下载的更新：{error}"),
            );
            return Err("无法读取已下载的更新，请重新下载。".to_owned());
        }
    };
    if sha256(&bytes) != cache.sha256 {
        let _ = tokio::fs::remove_file(&cache.path).await;
        mark_download_failed(
            &app,
            &pending,
            operation_id,
            "更新缓存完整性校验失败，请重新下载。".to_owned(),
        );
        return Err("更新缓存完整性校验失败，请重新下载。".to_owned());
    }

    #[cfg(windows)]
    {
        // Windows 的 install 成功路径不会返回；解包失败发生在退出 hook 之前，
        // 因而不会取消用户任务。缓存已完整读入内存，可在交给安装器前删除。
        let _ = tokio::fs::remove_file(&cache.path).await;
        if let Err(error) = update.install(bytes) {
            mark_download_failed(
                &app,
                &pending,
                operation_id,
                format!("更新安装失败：{error}"),
            );
            return Err(format!("更新安装失败：{error}"));
        }
        // 2.10.1 的成功路径已经退出进程；若后续实现意外返回，也不能在
        // cleanup_before_exit 之后继续调用 Tauri API。
        std::process::exit(0);
    }

    #[cfg(not(windows))]
    {
        // macOS 安装调用会返回：先确认安装成功，再取消会话并关闭 MCP，确保
        // 安装失败时当前应用仍可继续使用。
        if let Err(error) = update.install(bytes) {
            let _ = tokio::fs::remove_file(&cache.path).await;
            mark_download_failed(
                &app,
                &pending,
                operation_id,
                format!("更新安装失败：{error}"),
            );
            return Err(format!("更新安装失败：{error}"));
        }
        let _ = tokio::fs::remove_file(&cache.path).await;
        if let Err(error) = crate::app_exit::prepare_for_exit(&app).await {
            tracing::error!(%error, "更新已安装，退出清理失败，将强制重启以完成更新");
            app.state::<crate::app_exit::ExitState>().approve();
        }
        app.restart();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppUpdateDownloadSource, AppUpdateDownloadState, china_mirror_url, current_release,
        download_attempts, release_from_manifest, sha256,
    };

    #[test]
    fn development_build_uses_an_explicit_dev_release() {
        if super::RELEASE_TAG.is_none() {
            assert_eq!(current_release("0.0.1"), "v0.0.1-dev");
        }
    }

    #[test]
    fn download_states_use_the_frontend_contract_values() {
        assert_eq!(
            serde_json::to_value(AppUpdateDownloadState::Downloading).unwrap(),
            "downloading"
        );
        assert_eq!(
            serde_json::to_value(AppUpdateDownloadState::Ready).unwrap(),
            "ready"
        );
    }

    #[test]
    fn cached_update_digest_changes_with_the_downloaded_bytes() {
        assert_ne!(sha256(b"signed update"), sha256(b"changed update"));
    }

    #[test]
    fn automatic_download_tries_china_mirror_before_github() {
        let github = url::Url::parse(
            "https://github.com/chengliang4810/keen-code/releases/download/v1/KeenCode.zip",
        )
        .unwrap();
        let attempts = download_attempts(AppUpdateDownloadSource::Auto, &github).unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].source, AppUpdateDownloadSource::ChinaMirror);
        assert_eq!(attempts[1].source, AppUpdateDownloadSource::Github);
        assert_eq!(
            attempts[0].url.as_str(),
            "https://ghfast.top/https://github.com/chengliang4810/keen-code/releases/download/v1/KeenCode.zip"
        );
        assert_eq!(attempts[1].url, github);
    }

    #[test]
    fn explicit_download_sources_only_create_one_attempt() {
        let github = url::Url::parse(
            "https://github.com/chengliang4810/keen-code/releases/download/v1/KeenCode.zip",
        )
        .unwrap();
        let direct = download_attempts(AppUpdateDownloadSource::Github, &github).unwrap();
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].source, AppUpdateDownloadSource::Github);
        let mirror = download_attempts(AppUpdateDownloadSource::ChinaMirror, &github).unwrap();
        assert_eq!(mirror.len(), 1);
        assert_eq!(mirror[0].source, AppUpdateDownloadSource::ChinaMirror);
    }

    #[test]
    fn china_mirror_rejects_non_github_urls() {
        let other = url::Url::parse("https://example.com/update.zip").unwrap();
        assert!(china_mirror_url(&other).is_err());
    }

    #[test]
    fn reads_the_public_release_tag_from_the_updater_manifest() {
        let manifest = serde_json::json!({
            "release": "v20260730-49ad19b",
        });
        let url = url::Url::parse(
            "https://api.github.com/repos/chengliang4810/keen-code/releases/assets/123",
        )
        .unwrap();
        assert_eq!(
            release_from_manifest(&manifest, &url, "2026.730.1"),
            "v20260730-49ad19b"
        );
    }

    #[test]
    fn falls_back_to_a_tagged_download_url_for_non_annotated_manifests() {
        let url = url::Url::parse(
            "https://github.com/chengliang4810/keen-code/releases/download/v20260730-49ad19b/KeenCode.tar.gz",
        )
        .unwrap();
        assert_eq!(
            release_from_manifest(&serde_json::json!({}), &url, "2026.730.1"),
            "v20260730-49ad19b"
        );
    }
}
