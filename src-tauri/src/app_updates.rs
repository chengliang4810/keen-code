//! 应用版本信息与签名更新安装。

use serde::Serialize;
use std::{sync::Mutex, time::Duration};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};

/// 一次发布构建写入的对外版本标签；本地开发构建没有该变量。
const RELEASE_TAG: Option<&str> = option_env!("KEENCODE_RELEASE_TAG");

/// 尚未安装的已签名更新。检查和安装共用同一对象，避免重新信任远端元数据。
#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<Update>>);

/// 前端展示的当前版本与最近一次检查结果。
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
        }
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
) -> Result<std::sync::MutexGuard<'_, Option<Update>>, String> {
    pending
        .0
        .lock()
        .map_err(|_| "更新状态暂时不可用，请重试。".to_owned())
}

/// 只读取当前构建版本，不访问网络。
#[tauri::command]
pub fn app_update_info(app: AppHandle) -> AppUpdateStatus {
    AppUpdateStatus::current(&app)
}

/// 读取 GitHub Releases 的签名更新清单。
#[tauri::command]
pub async fn app_update_check(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<AppUpdateStatus, String> {
    let update = app
        .updater_builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| format!("更新服务配置无效：{error}"))?
        .check()
        .await
        .map_err(|error| format!("无法检查 GitHub Releases：{error}"))?;

    let mut status = AppUpdateStatus::current(&app);
    status.checked = true;
    if let Some(update) = update {
        status.available = true;
        status.latest_version = Some(update.version.clone());
        status.latest_release = Some(release_from_manifest(
            &update.raw_json,
            &update.download_url,
            &update.version,
        ));
        status.notes = update.body.clone().filter(|body| !body.trim().is_empty());
        status.published_at = update.date.map(|date| date.to_string());
        *pending_lock(&pending)? = Some(update);
    } else {
        *pending_lock(&pending)? = None;
    }
    Ok(status)
}

/// 下载、验签、安装最近一次检查到的更新，然后重启应用。
#[tauri::command]
pub async fn app_update_install(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<(), String> {
    let update = pending_lock(&pending)?
        .take()
        .ok_or_else(|| "没有可安装的更新，请先重新检查。".to_owned())?;
    let retry = update.clone();

    let bytes = match update.download(|_, _| {}, || {}).await {
        Ok(bytes) => bytes,
        Err(error) => {
            *pending_lock(&pending)? = Some(retry);
            return Err(format!("更新下载或签名校验失败：{error}"));
        }
    };

    if let Err(error) = crate::app_exit::prepare_for_exit(&app).await {
        *pending_lock(&pending)? = Some(retry);
        return Err(error);
    }
    if let Err(error) = update.install(bytes) {
        app.state::<crate::app_exit::ExitState>().reset();
        *pending_lock(&pending)? = Some(retry);
        return Err(format!("更新安装失败：{error}"));
    }

    app.restart();
}

#[cfg(test)]
mod tests {
    use super::{current_release, release_from_manifest};

    #[test]
    fn development_build_uses_an_explicit_dev_release() {
        if super::RELEASE_TAG.is_none() {
            assert_eq!(current_release("0.0.1"), "v0.0.1-dev");
        }
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
