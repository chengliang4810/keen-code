//! NativeHost 版本检查、签名下载和安装准备。
//!
//! 更新器不依赖桌面框架插件：清单和安装包都由 Rust 下载，安装包先用固定
//! minisign 公钥验证，再以同目录临时文件原子落盘；安装前再次验证哈希和签名。
//! 真正的替换/重启由 NativeHost 平台适配器执行，GPUI 只订阅 typed 状态事件。

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use crate::{app_settings::AppUpdateDownloadSource, native_paths::NativePaths};

/// 一次发布构建写入的对外版本标签；本地开发构建没有该变量。
/// 该标签只用于展示，版本比较只使用 `CARGO_PKG_VERSION` 和清单中的 SemVer。
const RELEASE_TAG: Option<&str> = option_env!("KEENCODE_RELEASE_TAG");
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CHINA_MIRROR_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const GITHUB_MIRROR_PREFIX: &str = "https://gh-proxy.org/";
const UPDATE_MANIFEST_URL: &str =
    "https://github.com/chengliang4810/keen-code/releases/latest/download/latest.json";
const UPDATE_MANIFEST_SCHEMA: &str = "keencode/native-update";
const UPDATE_MANIFEST_SCHEMA_VERSION: u32 = 1;
/// 发布签名公钥，编码后固定进 Rust 更新器。
const UPDATE_PUBLIC_KEY_ENCODED: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDg3QTQzNEQ4NzBDNkQ5MDIKUldRQzJjWncyRFNraDhuNHJPQ01EODNReHZraDFrRXBYMmpJWDhkUnFMM0xBTEdHbGVzRjAwL2EK";
const UPDATE_CACHE_FILE: &str = "updates/pending-update.bin";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppUpdateDownloadState {
    #[default]
    Idle,
    Downloading,
    Verifying,
    Ready,
    Installing,
    Failed,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateManifest {
    schema: String,
    schema_version: u32,
    /// 发布配置必须让该 SemVer 与构建时的 `CARGO_PKG_VERSION` 使用同一三段数字
    /// 规则并在每次发布时递增；`release` 的时间戳标签不能代替版本比较。
    version: String,
    release: Option<String>,
    notes: Option<String>,
    pub_date: Option<String>,
    platforms: BTreeMap<String, UpdateArtifact>,
}

impl UpdateManifest {
    fn validate(self) -> Result<Self, String> {
        if self.schema != UPDATE_MANIFEST_SCHEMA {
            return Err(format!(
                "更新清单 schema 不匹配，期望 {UPDATE_MANIFEST_SCHEMA}"
            ));
        }
        if self.schema_version != UPDATE_MANIFEST_SCHEMA_VERSION {
            return Err(format!(
                "更新清单 schemaVersion 不受支持，期望 {UPDATE_MANIFEST_SCHEMA_VERSION}"
            ));
        }
        parse_release_version(&self.version)
            .map_err(|error| format!("更新清单 version 无效：{error}"))?;
        Ok(self)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateArtifact {
    url: url::Url,
    signature: String,
}

#[derive(Clone)]
struct VerifiedUpdateCache {
    path: PathBuf,
    sha256: [u8; 32],
    signature: String,
}

#[derive(Default)]
struct PendingUpdateState {
    checked: bool,
    manifest: Option<UpdateManifest>,
    artifact: Option<UpdateArtifact>,
    download_state: AppUpdateDownloadState,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    download_source: Option<AppUpdateDownloadSource>,
    download_error: Option<String>,
    cache: Option<VerifiedUpdateCache>,
    available: bool,
    operation_id: u64,
}

/// 尚未安装的签名更新和后台下载状态。
#[derive(Clone, Default)]
pub struct PendingUpdate {
    state: Arc<Mutex<PendingUpdateState>>,
    // 更新检查、下载和安装都可能阻塞网络或平台回调；只串行更新流程，不能占用
    // NativeHost 的全局 admission 锁，否则会延迟 Stop/审批等实时控制信号。
    operation: Arc<Mutex<()>>,
}

impl PendingUpdate {
    fn operation_lock(&self) -> Result<MutexGuard<'_, ()>, String> {
        self.operation
            .lock()
            .map_err(|_| "更新操作锁暂时不可用，请重试。".to_owned())
    }
}

/// GPUI 展示的当前版本与最近一次检查、下载结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateStatus {
    pub current_version: String,
    pub current_release: String,
    pub checked: bool,
    pub available: bool,
    pub latest_version: Option<String>,
    pub latest_release: Option<String>,
    pub notes: Option<String>,
    pub published_at: Option<String>,
    pub download_state: AppUpdateDownloadState,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub download_source: Option<AppUpdateDownloadSource>,
    pub download_error: Option<String>,
}

/// 更新平台适配器。NativeHost 实现该 trait 后即可复用统一的验签/下载状态机。
pub trait NativeUpdateHost: Send + Sync {
    fn paths(&self) -> &NativePaths;
    fn current_version(&self) -> &str;
    fn download_source(&self) -> AppUpdateDownloadSource;
    fn emit_update_status(&self, status: AppUpdateStatus);
    /// 安装前停止活动 Turn、开发进程、transport 和诊断写入。
    fn prepare_for_update(&self) -> Result<(), String>;
    /// 平台适配器把已验证的安装包交给原生安装流程。
    fn install_verified_update(
        &self,
        package_path: &Path,
        bytes: &[u8],
        sha256: &[u8; 32],
        signature: &str,
    ) -> Result<(), String>;
    fn restart_after_update(&self) -> Result<(), String>;
}

impl AppUpdateStatus {
    fn current(version: &str) -> Self {
        Self {
            current_release: current_release(version),
            current_version: version.to_owned(),
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

    fn from_pending(version: &str, pending: &PendingUpdateState) -> Self {
        let mut status = Self::current(version);
        status.checked = pending.checked;
        status.download_state = pending.download_state;
        status.downloaded_bytes = pending.downloaded_bytes;
        status.total_bytes = pending.total_bytes;
        status.download_source = pending.download_source;
        status.download_error = pending.download_error.clone();
        if let Some(manifest) = pending.manifest.as_ref() {
            status.available = pending.available;
            status.latest_version = Some(manifest.version.clone());
            status.latest_release = Some(
                manifest
                    .release
                    .clone()
                    .unwrap_or_else(|| format!("v{}", manifest.version)),
            );
            status.notes = manifest
                .notes
                .clone()
                .filter(|value| !value.trim().is_empty());
            status.published_at = manifest.pub_date.clone();
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

fn pending_lock(pending: &PendingUpdate) -> Result<MutexGuard<'_, PendingUpdateState>, String> {
    pending
        .state
        .lock()
        .map_err(|_| "更新状态暂时不可用，请重试。".to_owned())
}

fn next_operation_id(state: &mut PendingUpdateState) -> u64 {
    state.operation_id = state.operation_id.wrapping_add(1);
    if state.operation_id == 0 {
        state.operation_id = 1;
    }
    state.operation_id
}

fn parse_release_version(value: &str) -> Result<(u64, u64, u64), String> {
    let value = value.trim().strip_prefix('v').unwrap_or(value.trim());
    let mut parts = value.split('.');
    let mut components = [0_u64; 3];
    for component in &mut components {
        let value = parts
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "更新版本必须是三段数字版本号。".to_owned())?;
        if value.len() > 1 && value.starts_with('0') {
            return Err("更新版本不能包含带前导零的数字段。".to_owned());
        }
        if !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("更新版本必须是三段数字版本号。".to_owned());
        }
        *component = value
            .parse::<u64>()
            .map_err(|_| "更新版本数字超出支持范围。".to_owned())?;
    }
    if parts.next().is_some() {
        return Err("更新版本必须是三段数字版本号。".to_owned());
    }
    Ok((components[0], components[1], components[2]))
}

fn is_newer_version(current: &str, candidate: &str) -> Result<bool, String> {
    let current = parse_release_version(current)?;
    let candidate = parse_release_version(candidate)?;
    Ok(candidate > current)
}

/// 只接受 HTTPS GitHub 地址作为清单的国内镜像输入，避免把任意 URL 拼进代理。
fn china_mirror_url(github_url: &url::Url) -> Result<url::Url, String> {
    if github_url.scheme() != "https" || github_url.host_str() != Some("github.com") {
        return Err("国内加速仅支持 GitHub 地址。".to_owned());
    }
    url::Url::parse(&format!("{GITHUB_MIRROR_PREFIX}{github_url}"))
        .map_err(|error| format!("国内加速地址无效：{error}"))
}

/// 按用户设置生成 GitHub 访问顺序；自动模式固定先国内加速、后 GitHub。
pub(crate) fn github_url_attempts(
    source: AppUpdateDownloadSource,
    github_url: &url::Url,
) -> Result<Vec<(AppUpdateDownloadSource, url::Url)>, String> {
    let github = (AppUpdateDownloadSource::Github, github_url.clone());
    match source {
        AppUpdateDownloadSource::Auto => Ok(vec![
            (
                AppUpdateDownloadSource::ChinaMirror,
                china_mirror_url(github_url)?,
            ),
            github,
        ]),
        AppUpdateDownloadSource::Github => Ok(vec![github]),
        AppUpdateDownloadSource::ChinaMirror => Ok(vec![(
            AppUpdateDownloadSource::ChinaMirror,
            china_mirror_url(github_url)?,
        )]),
    }
}

fn update_manifest_endpoints(
    source: AppUpdateDownloadSource,
) -> Result<Vec<(AppUpdateDownloadSource, url::Url)>, String> {
    let github = url::Url::parse(UPDATE_MANIFEST_URL)
        .map_err(|error| format!("GitHub 更新清单地址无效：{error}"))?;
    github_url_attempts(source, &github)
}

fn platform_key() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "darwin-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "darwin-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
    )))]
    {
        "unsupported"
    }
}

fn download_timeout(source: AppUpdateDownloadSource) -> Duration {
    match source {
        AppUpdateDownloadSource::ChinaMirror => CHINA_MIRROR_DOWNLOAD_TIMEOUT,
        AppUpdateDownloadSource::Github => UPDATE_DOWNLOAD_TIMEOUT,
        AppUpdateDownloadSource::Auto => UPDATE_DOWNLOAD_TIMEOUT,
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// 使用与下载/安装状态机相同的固定公钥校验更新签名。
///
/// 原生更新助手在主进程退出后也会重新调用该入口，避免助手复制一套可能漂移
/// 的公钥解析规则。
pub(crate) fn verify_update_signature(bytes: &[u8], encoded_signature: &str) -> Result<(), String> {
    let public_key_text = BASE64
        .decode(UPDATE_PUBLIC_KEY_ENCODED)
        .map_err(|error| format!("更新公钥编码无效：{error}"))?;
    let public_key_text = std::str::from_utf8(&public_key_text)
        .map_err(|error| format!("更新公钥文本无效：{error}"))?;
    let public_key =
        PublicKey::decode(public_key_text).map_err(|error| format!("更新公钥解析失败：{error}"))?;
    let signature = Signature::decode(encoded_signature)
        .map_err(|error| format!("更新签名解析失败：{error}"))?;
    public_key
        .verify(bytes, &signature, false)
        .map_err(|error| format!("更新签名校验失败：{error}"))
}

fn pending_status<H: NativeUpdateHost>(
    host: &H,
    pending: &PendingUpdate,
) -> Result<AppUpdateStatus, String> {
    let state = pending_lock(pending)?;
    Ok(AppUpdateStatus::from_pending(
        host.current_version(),
        &state,
    ))
}

fn publish_status<H: NativeUpdateHost>(host: &H, pending: &PendingUpdate) {
    if let Ok(status) = pending_status(host, pending) {
        host.emit_update_status(status);
    }
}

/// 读取当前版本及下载进度，不访问网络。
pub fn app_update_info<H: NativeUpdateHost>(
    host: &H,
    pending: &PendingUpdate,
) -> Result<AppUpdateStatus, String> {
    pending_status(host, pending)
}

fn parse_update_manifest(bytes: &[u8]) -> Result<UpdateManifest, String> {
    let manifest = serde_json::from_slice::<UpdateManifest>(bytes)
        .map_err(|error| format!("解析更新清单失败：{error}"))?;
    manifest.validate()
}

fn fetch_manifest(url: &url::Url) -> Result<UpdateManifest, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(UPDATE_CHECK_TIMEOUT)
        .build()
        .map_err(|error| format!("更新客户端初始化失败：{error}"))?;
    let response = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(|error| format!("读取更新清单失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("更新清单 HTTP 错误：{error}"))?;
    let bytes = response
        .bytes()
        .map_err(|error| format!("读取更新清单失败：{error}"))?;
    parse_update_manifest(&bytes)
}

/// 检查签名清单；成功后把对应平台的签名 artifact 记录为待下载对象。
pub fn app_update_check<H: NativeUpdateHost>(
    host: &H,
    pending: &PendingUpdate,
) -> Result<AppUpdateStatus, String> {
    let _operation = pending.operation_lock()?;
    let operation_id = {
        let mut state = pending_lock(pending)?;
        next_operation_id(&mut state)
    };
    let mut failures = Vec::new();
    let mut manifest = None;
    for (source, endpoint) in update_manifest_endpoints(host.download_source())? {
        match fetch_manifest(&endpoint) {
            Ok(value) => {
                manifest = Some((source, value));
                break;
            }
            Err(error) => failures.push(format!("{source:?}：{error}")),
        }
    }
    let Some((source, manifest)) = manifest else {
        return Err(failures.join("；"));
    };
    if !is_newer_version(host.current_version(), &manifest.version)? {
        let mut state = pending_lock(pending)?;
        if state.operation_id != operation_id {
            drop(state);
            return Err("更新检查已被新的操作取代".to_owned());
        }
        state.checked = true;
        state.download_source = Some(source);
        state.download_error = None;
        state.artifact = None;
        state.manifest = Some(manifest);
        state.available = false;
        state.download_state = AppUpdateDownloadState::Idle;
        state.downloaded_bytes = 0;
        state.total_bytes = None;
        state.cache = None;
        drop(state);
        let _ = fs::remove_file(cache_path(host.paths()));
        publish_status(host, pending);
        return pending_status(host, pending);
    }
    let artifact = manifest
        .platforms
        .get(platform_key())
        .cloned()
        .ok_or_else(|| format!("更新清单缺少当前平台 {}", platform_key()))?;
    if artifact.signature.trim().is_empty() {
        return Err("更新清单缺少签名，拒绝继续下载".to_owned());
    }

    let mut state = match pending_lock(pending) {
        Ok(state) => state,
        Err(error) => return Err(fail_operation(host, pending, operation_id, error)),
    };
    if state.operation_id != operation_id {
        drop(state);
        return Err("更新检查已被新的操作取代".to_owned());
    }
    state.checked = true;
    state.download_source = Some(source);
    state.download_error = None;
    state.artifact = Some(artifact);
    state.manifest = Some(manifest);
    state.download_state = AppUpdateDownloadState::Idle;
    state.downloaded_bytes = 0;
    state.total_bytes = None;
    state.cache = None;
    state.available = true;
    drop(state);
    let _ = fs::remove_file(cache_path(host.paths()));
    publish_status(host, pending);
    match pending_status(host, pending) {
        Ok(status) => Ok(status),
        Err(error) => Err(fail_operation(host, pending, operation_id, error)),
    }
}

fn download_bytes(url: &url::Url, source: AppUpdateDownloadSource) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(download_timeout(source))
        .build()
        .map_err(|error| format!("更新下载客户端初始化失败：{error}"))?;
    let response = client
        .get(url.clone())
        .send()
        .map_err(|error| format!("下载更新失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("更新下载 HTTP 错误：{error}"))?;
    let total = response.content_length();
    let bytes = response
        .bytes()
        .map_err(|error| format!("读取更新内容失败：{error}"))?
        .to_vec();
    if total.is_some_and(|expected| expected != bytes.len() as u64) {
        return Err("更新下载长度与响应 Content-Length 不一致".to_owned());
    }
    Ok(bytes)
}

fn cache_path(paths: &NativePaths) -> PathBuf {
    paths.data_root.join(UPDATE_CACHE_FILE)
}

/// 下载并验证当前平台更新。无签名或签名不匹配时绝不写入可安装缓存。
pub fn app_update_download<H: NativeUpdateHost>(
    host: &H,
    pending: &PendingUpdate,
) -> Result<AppUpdateStatus, String> {
    let _operation = pending.operation_lock()?;
    let (operation_id, artifact, source) = {
        let mut state = pending_lock(pending)?;
        if !state.available {
            return Err("没有比当前版本更新的版本，请先检查更新。".to_owned());
        }
        let artifact = state
            .artifact
            .clone()
            .ok_or_else(|| "没有待下载的更新，请先检查更新。".to_owned())?;
        let operation_id = next_operation_id(&mut state);
        state.download_state = AppUpdateDownloadState::Downloading;
        state.downloaded_bytes = 0;
        state.total_bytes = None;
        state.download_error = None;
        (
            operation_id,
            artifact,
            source_for_state(&state, host.download_source()),
        )
    };
    publish_status(host, pending);

    let attempts = match github_url_attempts(source, &artifact.url) {
        Ok(attempts) => attempts,
        Err(error) => return Err(fail_operation(host, pending, operation_id, error)),
    };
    let mut failures = Vec::new();
    let mut bytes = None;
    let mut selected_source = source;
    for (attempt_source, url) in attempts {
        match download_bytes(&url, attempt_source) {
            Ok(value) => {
                selected_source = attempt_source;
                bytes = Some(value);
                break;
            }
            Err(error) => failures.push(format!("{attempt_source:?}：{error}")),
        }
    }
    let Some(bytes) = bytes else {
        let message = failures.join("；");
        return Err(fail_operation(host, pending, operation_id, message));
    };
    {
        let mut state = match pending_lock(pending) {
            Ok(state) => state,
            Err(error) => return Err(fail_operation(host, pending, operation_id, error)),
        };
        if state.operation_id != operation_id {
            drop(state);
            return Err(fail_operation(
                host,
                pending,
                operation_id,
                "更新下载已被新的操作取代".to_owned(),
            ));
        }
        state.download_state = AppUpdateDownloadState::Verifying;
        state.downloaded_bytes = bytes.len() as u64;
        state.total_bytes = Some(bytes.len() as u64);
        state.download_source = Some(selected_source);
    }
    publish_status(host, pending);
    if let Err(error) = verify_update_signature(&bytes, &artifact.signature) {
        return Err(fail_operation(host, pending, operation_id, error));
    }

    let path = cache_path(host.paths());
    let Some(parent) = path.parent() else {
        return Err(fail_operation(
            host,
            pending,
            operation_id,
            "更新缓存路径缺少父目录".to_owned(),
        ));
    };
    if let Err(error) = fs::create_dir_all(parent) {
        return Err(fail_operation(
            host,
            pending,
            operation_id,
            format!("创建更新缓存目录失败：{error}"),
        ));
    }
    if let Err(error) = remove_cache_file(host.paths()) {
        return Err(fail_operation(host, pending, operation_id, error));
    }
    let mut temporary = match tempfile::NamedTempFile::new_in(parent) {
        Ok(temporary) => temporary,
        Err(error) => {
            return Err(fail_operation(
                host,
                pending,
                operation_id,
                format!("创建更新临时文件失败：{error}"),
            ));
        }
    };
    use std::io::Write;
    if let Err(error) = temporary
        .write_all(&bytes)
        .and_then(|_| temporary.as_file().sync_all())
    {
        return Err(fail_operation(
            host,
            pending,
            operation_id,
            format!("写入更新临时文件失败：{error}"),
        ));
    }
    if let Err(error) = temporary.persist(&path) {
        return Err(fail_operation(
            host,
            pending,
            operation_id,
            format!("提交更新缓存失败：{}", error.error),
        ));
    }

    let mut state = match pending_lock(pending) {
        Ok(state) => state,
        Err(error) => return Err(fail_operation(host, pending, operation_id, error)),
    };
    if state.operation_id != operation_id {
        drop(state);
        return Err(fail_operation(
            host,
            pending,
            operation_id,
            "更新下载已被新的操作取代".to_owned(),
        ));
    }
    state.download_state = AppUpdateDownloadState::Ready;
    state.download_error = None;
    state.cache = Some(VerifiedUpdateCache {
        path,
        sha256: sha256(&bytes),
        signature: artifact.signature,
    });
    drop(state);
    publish_status(host, pending);
    match pending_status(host, pending) {
        Ok(status) => Ok(status),
        Err(error) => Err(fail_operation(host, pending, operation_id, error)),
    }
}

fn source_for_state(
    state: &PendingUpdateState,
    default: AppUpdateDownloadSource,
) -> AppUpdateDownloadSource {
    state.download_source.unwrap_or(default)
}

fn remove_cache_file(paths: &NativePaths) -> Result<(), String> {
    match fs::remove_file(cache_path(paths)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("清理旧更新缓存失败：{error}")),
    }
}

fn mark_failed(pending: &PendingUpdate, operation_id: u64, error: String) -> bool {
    if let Ok(mut state) = pending.state.lock()
        && state.operation_id == operation_id
    {
        state.download_state = AppUpdateDownloadState::Failed;
        state.download_error = Some(error);
        state.cache = None;
        return true;
    }
    false
}

fn fail_operation<H: NativeUpdateHost>(
    host: &H,
    pending: &PendingUpdate,
    operation_id: u64,
    error: String,
) -> String {
    if mark_failed(pending, operation_id, error.clone()) {
        let _ = remove_cache_file(host.paths());
    }
    publish_status(host, pending);
    error
}

/// 安装前再次校验缓存文件的哈希与 minisign 签名，然后交给 NativeHost 原生安装器。
pub fn app_update_install<H: NativeUpdateHost>(
    host: &H,
    pending: &PendingUpdate,
) -> Result<(), String> {
    let _operation = pending.operation_lock()?;
    let (operation_id, cache) = {
        let mut state = pending_lock(pending)?;
        if state.download_state != AppUpdateDownloadState::Ready {
            return Err("更新尚未下载并校验完成，请稍后重试。".to_owned());
        }
        let operation_id = next_operation_id(&mut state);
        let Some(cache) = state.cache.clone() else {
            drop(state);
            return Err(fail_operation(
                host,
                pending,
                operation_id,
                "更新缓存不存在".to_owned(),
            ));
        };
        state.download_state = AppUpdateDownloadState::Installing;
        (operation_id, cache)
    };
    publish_status(host, pending);
    let bytes = match fs::read(&cache.path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(fail_operation(
                host,
                pending,
                operation_id,
                format!("读取更新缓存失败：{error}"),
            ));
        }
    };
    if sha256(&bytes) != cache.sha256 {
        return Err(fail_operation(
            host,
            pending,
            operation_id,
            "更新缓存哈希校验失败，请重新下载。".to_owned(),
        ));
    }
    if let Err(error) = verify_update_signature(&bytes, &cache.signature) {
        return Err(fail_operation(host, pending, operation_id, error));
    }
    if let Err(error) = host.prepare_for_update() {
        return Err(fail_operation(host, pending, operation_id, error));
    }
    if let Err(error) =
        host.install_verified_update(&cache.path, &bytes, &cache.sha256, &cache.signature)
    {
        return Err(fail_operation(host, pending, operation_id, error));
    }
    let _ = fs::remove_file(&cache.path);
    if let Err(error) = host.restart_after_update() {
        return Err(fail_operation(host, pending, operation_id, error));
    }
    let mut state = match pending_lock(pending) {
        Ok(state) => state,
        Err(error) => return Err(fail_operation(host, pending, operation_id, error)),
    };
    if state.operation_id != operation_id {
        drop(state);
        return Err(fail_operation(
            host,
            pending,
            operation_id,
            "更新安装已被新的操作取代".to_owned(),
        ));
    }
    state.download_state = AppUpdateDownloadState::Idle;
    state.download_error = None;
    state.available = false;
    state.cache = None;
    drop(state);
    publish_status(host, pending);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AppUpdateDownloadSource, AppUpdateDownloadState, PendingUpdate, VerifiedUpdateCache,
        current_release, github_url_attempts, is_newer_version, mark_failed, parse_release_version,
        parse_update_manifest, pending_lock, sha256,
    };
    use std::path::PathBuf;

    fn valid_manifest_fixture() -> serde_json::Value {
        serde_json::json!({
            "schema": "keencode/native-update",
            "schemaVersion": 1,
            "version": "1.2.4",
            "release": "v20261005-abcdef0",
            "notes": "native update fixture",
            "pubDate": "2026-10-05T00:00:00Z",
            "platforms": {
                "windows-x86_64": {
                    "url": "https://github.com/chengliang4810/keen-code/releases/download/v20261005-abcdef0/KeenCode.exe",
                    "signature": "fixture-signature"
                }
            }
        })
    }

    #[test]
    fn development_build_uses_an_explicit_dev_release() {
        if super::RELEASE_TAG.is_none() {
            assert_eq!(current_release("0.0.1"), "v0.0.1-dev");
        }
    }

    #[test]
    fn download_states_use_native_values() {
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
    fn cached_update_digest_changes_with_downloaded_bytes() {
        assert_ne!(sha256(b"signed update"), sha256(b"changed update"));
    }

    #[test]
    fn automatic_download_tries_china_mirror_before_github() {
        let github = url::Url::parse(
            "https://github.com/chengliang4810/keen-code/releases/download/v1/KeenCode.zip",
        )
        .unwrap();
        let attempts = github_url_attempts(AppUpdateDownloadSource::Auto, &github).unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].0, AppUpdateDownloadSource::ChinaMirror);
        assert_eq!(attempts[1].0, AppUpdateDownloadSource::Github);
    }

    #[test]
    fn release_version_comparison_rejects_same_or_older_versions() {
        assert_eq!(parse_release_version("v1.2.3").unwrap(), (1, 2, 3));
        assert!(is_newer_version("1.2.3", "1.2.4").unwrap());
        assert!(!is_newer_version("1.2.3", "1.2.3").unwrap());
        assert!(!is_newer_version("1.2.3", "1.2.2").unwrap());
        assert!(parse_release_version("1.2").is_err());
        assert!(parse_release_version("1.02.3").is_err());
        assert!(parse_release_version("1.2.3-beta").is_err());
    }

    #[test]
    fn native_update_manifest_fixture_accepts_the_current_contract() {
        let bytes = serde_json::to_vec(&valid_manifest_fixture()).unwrap();
        let manifest = parse_update_manifest(&bytes).unwrap();
        assert_eq!(manifest.schema, super::UPDATE_MANIFEST_SCHEMA);
        assert_eq!(
            manifest.schema_version,
            super::UPDATE_MANIFEST_SCHEMA_VERSION
        );
    }

    #[test]
    fn native_update_manifest_fixture_rejects_unknown_and_mismatched_contracts() {
        let mut unknown = valid_manifest_fixture();
        unknown["unexpected"] = serde_json::json!(true);
        let unknown_bytes = serde_json::to_vec(&unknown).unwrap();
        assert!(parse_update_manifest(&unknown_bytes).is_err());

        let mut wrong_schema = valid_manifest_fixture();
        wrong_schema["schema"] = serde_json::json!("tauri-updater");
        let wrong_schema_bytes = serde_json::to_vec(&wrong_schema).unwrap();
        assert!(parse_update_manifest(&wrong_schema_bytes).is_err());

        let mut wrong_version = valid_manifest_fixture();
        wrong_version["schemaVersion"] = serde_json::json!(2);
        let wrong_version_bytes = serde_json::to_vec(&wrong_version).unwrap();
        assert!(parse_update_manifest(&wrong_version_bytes).is_err());
    }

    #[test]
    fn failed_operation_is_retryable_and_stale_failure_cannot_overwrite_newer_work() {
        let pending = PendingUpdate::default();
        {
            let mut state = pending_lock(&pending).unwrap();
            state.available = true;
            state.operation_id = 7;
            state.download_state = AppUpdateDownloadState::Installing;
            state.cache = Some(VerifiedUpdateCache {
                path: PathBuf::from("missing-update.bin"),
                sha256: sha256(b"update"),
                signature: "signature".to_owned(),
            });
        }

        assert!(mark_failed(&pending, 7, "安装失败，请重试。".to_owned()));
        {
            let state = pending_lock(&pending).unwrap();
            assert_eq!(state.download_state, AppUpdateDownloadState::Failed);
            assert_eq!(state.download_error.as_deref(), Some("安装失败，请重试。"));
            assert!(state.cache.is_none());
            assert!(state.available);
        }

        {
            let mut state = pending_lock(&pending).unwrap();
            state.operation_id = 8;
            state.download_state = AppUpdateDownloadState::Ready;
        }
        assert!(!mark_failed(
            &pending,
            7,
            "旧操作失败，不应覆盖新操作。".to_owned()
        ));
        assert_eq!(
            pending_lock(&pending).unwrap().download_state,
            AppUpdateDownloadState::Ready
        );
    }
}
