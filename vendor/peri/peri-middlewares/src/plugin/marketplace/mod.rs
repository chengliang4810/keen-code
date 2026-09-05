use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use peri_acp_types::plugin::is_windows_reserved_device_name;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::plugin::types::{MarketplaceManifest, MarketplaceSource, PluginAuthor, PluginId};

mod fetch;
mod manager;

/// 供 marketplace 与外部插件安装共用 Git 临时 checkout 原子提升入口。
pub(crate) use fetch::{clone_git_checkout, git_checkout_is_valid};
pub use manager::MarketplaceManager;

// ─── Types ────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum MarketplaceError {
    #[error("Git 操作失败: {0}")]
    GitFailed(String),
    #[error("HTTP 请求失败: {0}")]
    HttpFailed(String),
    /// marketplace 名称不满足共享标识或跨平台缓存路径契约。
    #[error("marketplace 名称无效: {0}")]
    InvalidName(String),
    #[error("JSON 解析失败: {0}")]
    ParseFailed(String),
    #[error("marketplace.json 未找到: {path}")]
    ManifestNotFound { path: String },
    #[error("NPM 操作失败: {0}")]
    NpmFailed(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarketplaceStatus {
    Cached,
    Fetching,
    Fresh,
    Stale(String),
    NotFetched,
}

pub struct MarketplaceEntry {
    pub name: String,
    pub source: MarketplaceSource,
    pub manifest: Option<MarketplaceManifest>,
    pub status: MarketplaceStatus,
    pub last_updated: Option<DateTime<Utc>>,
    pub auto_update: bool,
}

#[derive(Debug, Clone)]
pub struct AvailablePlugin {
    pub name: String,
    pub description: String,
    pub version: String,
    pub marketplace: String,
    pub source: serde_json::Value,
    pub author: Option<PluginAuthor>,
    pub category: Option<String>,
    pub homepage: Option<String>,
}

#[derive(Debug, Clone)]
pub enum MarketplaceRefreshEvent {
    Updated {
        index: usize,
        name: String,
    },
    Failed {
        index: usize,
        name: String,
        error: String,
    },
}

// ─── Utility Functions ────────────────────────────────────────────────

pub fn find_marketplace_json(dir: &Path) -> Option<PathBuf> {
    let root = dir.join("marketplace.json");
    if root.exists() {
        return Some(root);
    }
    let subdir = dir.join(".claude-plugin").join("marketplace.json");
    if subdir.exists() {
        return Some(subdir);
    }
    None
}

pub fn read_manifest_from_path(path: &Path) -> Result<MarketplaceManifest, MarketplaceError> {
    let content = std::fs::read_to_string(path)?;
    serde_json::from_str(&content).map_err(|e| MarketplaceError::ParseFailed(e.to_string()))
}

const MAX_NPM_PACKAGE_BYTES: usize = 214;
/// Windows/NTFS 单个路径组件的最大字节数；缓存键必须始终低于此上限。
const MAX_CACHE_KEY_BYTES: usize = 255;
/// URL marketplace 缓存文件的扩展名长度也计入 Windows 单组件上限。
const MARKETPLACE_CACHE_FILE_EXTENSION: &str = ".json";
const MAX_CACHE_FILE_KEY_BYTES: usize =
    MAX_CACHE_KEY_BYTES - MARKETPLACE_CACHE_FILE_EXTENSION.len();
/// NPM 缓存/namespace 的域前缀，避免它与普通 marketplace 名称混淆。
const NPM_NAMESPACE_PREFIX: &str = "npm-";
/// 超长 NPM namespace 使用独立域前缀，和短名称的十六进制编码空间分离。
const NPM_NAMESPACE_HASH_PREFIX: &str = "npm-sha256-";
/// NPM namespace 契约异常收紧时的短保底值，仍属于可识别的 NPM 编码域。
const NPM_NAMESPACE_FALLBACK: &str = "npm-00";
/// 普通 marketplace 若占用 NPM namespace 前缀，必须进入独立的 marketplace 名空间。
const MARKETPLACE_NAMESPACE_PREFIX: &str = "mkt-";
/// URL marketplace 缓存使用独立目录，不能与 Git/NPM marketplace 目录重叠。
const URL_MARKETPLACE_CACHE_DIR: &str = "url";

/// 校验 NPM 包名，确保它既符合支持的包名形态，也不会成为命令行选项或路径。
pub(crate) fn validate_npm_package(package: &str) -> Result<(), String> {
    if package.is_empty() {
        return Err("NPM 包名不能为空".into());
    }
    if package.len() > MAX_NPM_PACKAGE_BYTES {
        return Err(format!("NPM 包名不能超过 {MAX_NPM_PACKAGE_BYTES} 个字节"));
    }
    if package.starts_with('-') {
        return Err("NPM 包名不能以 '-' 开头".into());
    }
    if package.contains('\\') {
        return Err("NPM 包名不能包含反斜杠".into());
    }
    if package
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err("NPM 包名不能包含空白或控制字符".into());
    }

    let components: Vec<&str> = if let Some(scoped) = package.strip_prefix('@') {
        let (scope, name) = scoped
            .split_once('/')
            .ok_or_else(|| "作用域 NPM 包名必须为 @scope/name".to_string())?;
        if scope.is_empty() || name.is_empty() || name.contains('/') {
            return Err("作用域 NPM 包名必须为单个 @scope/name".into());
        }
        vec![scope, name]
    } else {
        if package.contains('/') {
            return Err("未作用域 NPM 包名不能包含路径分隔符".into());
        }
        vec![package]
    };

    for component in components {
        if component == "." || component == ".." {
            return Err("NPM 包名不能使用 '.' 或 '..' 路径段".into());
        }
        if component.starts_with('.') || component.starts_with('_') {
            return Err("NPM 包名不能以 '.' 或 '_' 开头".into());
        }
        if !component.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        }) {
            return Err("NPM 包名只能包含小写字母、数字、'.'、'_' 和 '-'".into());
        }
    }

    Ok(())
}

/// 校验用于 marketplace 缓存的相对名称，拒绝跨平台路径逃逸字符。
pub(crate) fn validate_marketplace_cache_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("marketplace 名称不能为空".into());
    }
    if name.len() > MAX_CACHE_KEY_BYTES {
        return Err(format!(
            "marketplace 缓存名称不能超过 {MAX_CACHE_KEY_BYTES} 个字节"
        ));
    }
    if name.starts_with('/') || name.starts_with('\\') || name.contains('\\') {
        return Err("marketplace 名称必须是相对路径，不能包含反斜杠".into());
    }
    if name
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err("marketplace 名称不能包含空白或控制字符".into());
    }

    for (component_index, component) in name.split('/').enumerate() {
        if component.is_empty() || component == "." || component == ".." {
            return Err("marketplace 名称包含空或不安全的路径段".into());
        }
        if component.ends_with('.') || is_windows_reserved_device_name(component) {
            return Err("marketplace 名称包含 Windows 不安全的路径段".into());
        }
        for (byte_index, byte) in component.bytes().enumerate() {
            let allowed = byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'-')
                || (component_index == 0 && byte_index == 0 && byte == b'@');
            if !allowed {
                return Err("marketplace 名称包含不安全字符".into());
            }
        }
    }

    Ok(())
}

/// 将一个字符串编码为只含十六进制 ASCII 的稳定表示。
fn hex_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// 使用固定域前缀计算稳定 SHA-256，避免不同用途的哈希键相互碰撞。
fn stable_sha256_hex(domain: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"keencode.marketplace.v1\0");
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

/// 将任意外部字符串转换为安全、稳定且长度受控的单个存储组件。
///
/// 可直接使用的组件保留原文，其他值使用波浪号域前缀编码；过长编码再
/// 使用带用途域分离的 SHA-256。由于原始安全组件禁止波浪号，两种空间不会
/// 发生碰撞，且调用方仍可在记录中保留未经改写的原始值。
pub(crate) fn bounded_storage_component(value: &str, domain: &str) -> String {
    if is_safe_storage_component(value) {
        return value.to_owned();
    }

    let prefix = format!("~{domain}-");
    let encoded = format!("{prefix}{}", hex_encode(value));
    if encoded.len() <= MAX_CACHE_KEY_BYTES {
        return encoded;
    }

    format!(
        "{prefix}sha256-{}",
        stable_sha256_hex(&format!("storage:{domain}"), value)
    )
}

/// 判断字符串能否原样作为跨平台安全的单个路径组件。
fn is_safe_storage_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CACHE_KEY_BYTES
        && !value.ends_with('.')
        && !is_windows_reserved_device_name(value)
        && !value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// 判断名称是否正好占用路径 marketplace 的编码键命名空间。
fn is_encoded_marketplace_key(name: &str) -> bool {
    let prefix_len = "marketplace-".len();
    let Some(encoded) = name
        .get(prefix_len..)
        .filter(|_| name[..prefix_len].eq_ignore_ascii_case("marketplace-"))
    else {
        return false;
    };
    !encoded.is_empty()
        && encoded.len() % 2 == 0
        && encoded.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 判断名称是否占用 NPM namespace 的保留域；比较时必须与 PluginId 一样忽略 ASCII 大小写。
fn has_npm_namespace_prefix(value: &str) -> bool {
    value
        .get(..NPM_NAMESPACE_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(NPM_NAMESPACE_PREFIX))
}

/// 判断一个 marketplace 名称是否是本模块生成的 NPM namespace。
fn is_npm_namespace(value: &str) -> bool {
    if !has_npm_namespace_prefix(value) {
        return false;
    }

    let suffix = &value[NPM_NAMESPACE_PREFIX.len()..];
    let is_hex = |candidate: &str| {
        !candidate.is_empty()
            && candidate.len().is_multiple_of(2)
            && candidate.bytes().all(|byte| byte.is_ascii_hexdigit())
    };

    if is_hex(suffix) {
        return true;
    }

    let Some(hash_suffix) = value.get(NPM_NAMESPACE_HASH_PREFIX.len()..).filter(|_| {
        value[..NPM_NAMESPACE_HASH_PREFIX.len()].eq_ignore_ascii_case(NPM_NAMESPACE_HASH_PREFIX)
    }) else {
        return false;
    };
    let invalid_digest = hash_suffix
        .get(.."invalid-".len())
        .filter(|prefix| prefix.eq_ignore_ascii_case("invalid-"))
        .and_then(|_| hash_suffix.get("invalid-".len()..));
    (hash_suffix.len() == 64 && hash_suffix.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || invalid_digest.is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// 校验已经生成的单组件缓存键，允许内部使用的 `~` 域前缀。
fn validate_cache_key_component(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("缓存键不能为空".into());
    }
    if key.len() > MAX_CACHE_KEY_BYTES {
        return Err(format!("缓存键不能超过 {MAX_CACHE_KEY_BYTES} 个字节"));
    }
    if key.starts_with('/')
        || key.starts_with('\\')
        || key.contains('/')
        || key.contains('\\')
        || key
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err("缓存键必须是安全的单个路径组件".into());
    }
    if key.ends_with('.') || is_windows_reserved_device_name(key) {
        return Err("缓存键包含 Windows 不安全的路径段".into());
    }
    if !key.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~' | b'@')
    }) {
        return Err("缓存键包含不安全字符".into());
    }
    Ok(())
}

/// 返回 marketplace 的跨平台 identity；仅折叠 ASCII 大小写，保持展示值的字符域不变。
pub(crate) fn marketplace_identity(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// 按 marketplace identity 比较名称，避免调用方自行使用大小写敏感比较。
pub(crate) fn marketplace_names_equal(left: &str, right: &str) -> bool {
    marketplace_identity(left) == marketplace_identity(right)
}

/// 将包含路径段的 marketplace 名称编码为单个、无碰撞且长度受控的缓存目录名。
pub(crate) fn marketplace_cache_key(name: &str) -> Result<String, String> {
    let identity = marketplace_identity(name);
    validate_marketplace_cache_name(&identity)?;
    let key = if identity.contains('/') {
        let encoded = format!("marketplace-{}", hex_encode(&identity));
        if encoded.len() <= MAX_CACHE_KEY_BYTES {
            encoded
        } else {
            format!(
                "~path-sha256-{}",
                stable_sha256_hex("cache-path", &identity)
            )
        }
    } else if is_encoded_marketplace_key(&identity) {
        // 路径编码键以 `marketplace-` 开头；用不可由原始名称生成的 `~` 域
        // 转义同形普通名称，保证原始名称与编码结果永不相同。
        let encoded = format!("~raw-{}", hex_encode(&identity));
        if encoded.len() <= MAX_CACHE_KEY_BYTES {
            encoded
        } else {
            format!(
                "~raw-sha256-{}",
                stable_sha256_hex("cache-raw-name", &identity)
            )
        }
    } else if has_npm_namespace_prefix(&identity) {
        // `npm-*` 是 NPM 的保留域；普通 Git/URL marketplace 使用不可由原始
        // 名称产生的 `~` 键，避免与 NPM 目录发生跨来源覆盖。
        let encoded = format!("~raw-{}", hex_encode(&identity));
        if encoded.len() <= MAX_CACHE_KEY_BYTES {
            encoded
        } else {
            format!(
                "~raw-sha256-{}",
                stable_sha256_hex("cache-raw-name", &identity)
            )
        }
    } else {
        identity
    };
    validate_cache_key_component(&key)?;
    Ok(key)
}

/// 返回安全的 marketplace 缓存目录路径；路径不会直接使用未校验的名称。
pub(crate) fn marketplace_cache_dir(cache_base: &Path, name: &str) -> Result<PathBuf, String> {
    Ok(cache_base.join(marketplace_cache_key(name)?))
}

/// 按已经生成的唯一键返回缓存目录，供 NPM namespace 等调用方复用同一落盘入口。
pub(crate) fn marketplace_cache_dir_from_key(
    cache_base: &Path,
    key: &str,
) -> Result<PathBuf, String> {
    let identity = marketplace_identity(key);
    validate_cache_key_component(&identity)?;
    Ok(cache_base.join(identity))
}

/// 返回安全的 marketplace JSON 缓存路径。
pub(crate) fn marketplace_cache_file(cache_base: &Path, name: &str) -> Result<PathBuf, String> {
    let key = marketplace_cache_key(name)?;
    let key = if key.len() <= MAX_CACHE_FILE_KEY_BYTES {
        key
    } else {
        // 文件扩展名占 5 字节；不能把目录键的 255 字节上限直接复用到文件。
        let identity = marketplace_identity(name);
        format!(
            "~file-sha256-{}",
            stable_sha256_hex("cache-file", &identity)
        )
    };
    let file_name = format!("{key}{MARKETPLACE_CACHE_FILE_EXTENSION}");
    debug_assert!(file_name.len() <= MAX_CACHE_KEY_BYTES);
    validate_cache_key_component(&file_name)?;
    // URL 清单是文件，而 Git/NPM marketplace 是目录；独立根目录还可以
    // 防止 `foo.json` 目录与 `foo.json` URL 文件在同一缓存根下互相覆盖。
    Ok(cache_base.join(URL_MARKETPLACE_CACHE_DIR).join(file_name))
}

/// 返回 NPM 包对应的缓存目录；目录名同时是可用于 `PluginId` 的 namespace。
pub(crate) fn npm_cache_dir(cache_base: &Path, package: &str) -> Result<PathBuf, String> {
    validate_npm_package(package)?;
    marketplace_cache_dir_from_key(cache_base, &npm_marketplace_namespace(package))
}

/// 按插件 ID 中的 marketplace namespace 定位缓存目录。
///
/// NPM namespace 与普通 marketplace 共用一个缓存根，但通过保留前缀和
/// 本入口区分来源；普通名称仍统一经过安全 key helper。
pub(crate) fn marketplace_cache_dir_for_namespace(
    cache_base: &Path,
    namespace: &str,
) -> Result<PathBuf, String> {
    if is_npm_namespace(namespace) {
        marketplace_cache_dir_from_key(cache_base, namespace)
    } else {
        marketplace_cache_dir(cache_base, namespace)
    }
}

/// 为非 NPM 来源生成不占用 NPM namespace 域的 marketplace 名称。
pub(crate) fn non_npm_marketplace_name(name: &str) -> String {
    let identity = marketplace_identity(name);
    let is_reserved_namespace = has_npm_namespace_prefix(&identity)
        || is_encoded_marketplace_key(&identity)
        || identity
            .get(..MARKETPLACE_NAMESPACE_PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(MARKETPLACE_NAMESPACE_PREFIX));

    // Marketplace 名称最终会作为 PluginId 的 marketplace 组件；统一让
    // PluginId 校验长度和字符契约，避免这里复制它的 127 字节限制。
    // 仅将 identity 用于比较和安全性判断；展示名称保留来源中的合法大小写。
    if !is_reserved_namespace
        && PluginId::from_components("plugin", Some(name))
            .is_ok_and(|id| id.marketplace.as_deref() == Some(name))
    {
        return name.to_owned();
    }

    let encoded = format!("{MARKETPLACE_NAMESPACE_PREFIX}{}", hex_encode(&identity));
    if PluginId::from_components("plugin", Some(&encoded)).is_ok() {
        return encoded;
    }

    format!(
        "{MARKETPLACE_NAMESPACE_PREFIX}sha256-{}",
        stable_sha256_hex("marketplace-name", &identity)
    )
}

/// 按共享 `PluginId` 规则比较两个插件名，忽略 ASCII 大小写差异。
pub(crate) fn plugin_names_equal(left: &str, right: &str) -> bool {
    PluginId::from_components(left, None)
        .ok()
        .zip(PluginId::from_components(right, None).ok())
        .is_some_and(|(left, right)| left == right)
}

/// 将 NPM 包映射为稳定、跨平台且满足 `PluginId` 上限的 marketplace namespace。
pub(crate) fn npm_marketplace_namespace(package: &str) -> String {
    let identity = marketplace_identity(package);
    let candidate = if validate_npm_package(package).is_ok() {
        format!("{NPM_NAMESPACE_PREFIX}{}", hex_encode(&identity))
    } else {
        format!(
            "{NPM_NAMESPACE_HASH_PREFIX}invalid-{}",
            stable_sha256_hex("npm-namespace-invalid", &identity)
        )
    };

    // PluginId 是 marketplace 标识长度的唯一契约来源，不在此处复制常量。
    if let Ok(id) = PluginId::from_components("npm", Some(&candidate)) {
        return id
            .marketplace
            .expect("带 marketplace 的 PluginId 必须保留 marketplace 字段");
    }

    let hashed = format!(
        "{NPM_NAMESPACE_HASH_PREFIX}{}",
        stable_sha256_hex("npm-namespace", &identity)
    );
    if let Ok(id) = PluginId::from_components("npm", Some(&hashed)) {
        return id
            .marketplace
            .expect("带 marketplace 的 PluginId 必须保留 marketplace 字段");
    }

    // 这是 PluginId 契约发生不兼容收紧时的最后防线：候选值仍必须经过同一
    // 校验入口，绝不能退回可能超过长度上限的十六进制编码。若连固定短值也
    // 不再合法，说明不存在可安全表达的 NPM namespace，应尽早暴露契约冲突。
    PluginId::from_components("npm", Some(NPM_NAMESPACE_FALLBACK))
        .expect("PluginId 契约无法接受任何安全的 NPM namespace")
        .marketplace
        .expect("带 marketplace 的 PluginId 必须保留 marketplace 字段")
}

// ─── Parse & Refresh ──────────────────────────────────────────────────

/// 解析用户输入的 marketplace source 字符串
pub fn parse_marketplace_input(input: &str) -> Result<MarketplaceSource, String> {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return Err("输入不能为空".to_string());
    }

    // 1. Git SSH URLs: user@host:path 或 user@host:path.git
    if let Some(ssh_match) = trimmed.strip_prefix("git@") {
        if let Some((host, path)) = ssh_match.split_once(':') {
            let path = path.strip_suffix(".git").unwrap_or(path);
            return Ok(MarketplaceSource::GitHub {
                repo: format!("git@{}:{}", host, path),
            });
        }
    }

    // 2. HTTP/HTTPS URLs
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        if trimmed.contains("github.com/") {
            let parts: Vec<&str> = trimmed.split('/').collect();
            if parts.len() >= 5 {
                let owner = parts[3];
                let repo = parts[4].trim_end_matches(".git");
                return Ok(MarketplaceSource::GitHub {
                    repo: format!("{}/{}", owner, repo),
                });
            }
        }
        return Ok(MarketplaceSource::Url {
            url: trimmed.to_string(),
        });
    }

    // 3. 本地路径：./, ../, /, ~ 开头
    if trimmed.starts_with("./")
        || trimmed.starts_with("../")
        || trimmed.starts_with('/')
        || trimmed.starts_with('~')
        || trimmed.starts_with(".\\")
        || trimmed.starts_with("..\\")
        || (trimmed.len() >= 3 && trimmed.as_bytes()[1] == b'\\')
        || (trimmed.len() >= 2
            && trimmed.as_bytes()[0].is_ascii_alphabetic()
            && trimmed.as_bytes()[1] == b':')
    {
        let path = shellexpand::tilde(trimmed).to_string();
        let path_obj = Path::new(&path);
        if path_obj.ends_with(".json") || path_obj.extension().is_some_and(|e| e == "json") {
            return Ok(MarketplaceSource::File { path });
        } else {
            return Ok(MarketplaceSource::Directory { path });
        }
    }

    // 4. GitHub shorthand: owner/repo
    if trimmed.contains('/') && !trimmed.starts_with('@') {
        let parts: Vec<&str> = trimmed.split('/').collect();
        if parts.len() == 2 {
            return Ok(MarketplaceSource::GitHub {
                repo: trimmed.to_string(),
            });
        }
    }

    // 5. NPM package: @scope/name 或 name
    if trimmed.starts_with('@') || !trimmed.contains('/') {
        validate_npm_package(trimmed)?;
        return Ok(MarketplaceSource::Npm {
            package: trimmed.to_string(),
        });
    }

    Err(format!("无法识别的 marketplace source: {}", trimmed))
}

/// 刷新单个 marketplace 的缓存，返回 manifest 和缓存路径
pub async fn refresh_marketplace(
    source: &MarketplaceSource,
    name: &str,
) -> Result<(MarketplaceManifest, String), MarketplaceError> {
    let cache_base = crate::plugin::config::marketplaces_cache_dir();
    let auto_update = true;

    let manifest = match source {
        MarketplaceSource::GitHub { repo } => {
            fetch::fetch_github(name, repo, &cache_base, auto_update).await?
        }
        MarketplaceSource::Git { url } => {
            fetch::fetch_git(name, url, &cache_base, auto_update).await?
        }
        MarketplaceSource::Url { url } => fetch::fetch_url(name, url, &cache_base).await?,
        MarketplaceSource::File { path } => {
            let path = path.clone();
            tokio::task::spawn_blocking(move || fetch::read_file(Path::new(&path)))
                .await
                .expect("spawn_blocking panicked")?
        }
        MarketplaceSource::Directory { path } => {
            let path = path.clone();
            tokio::task::spawn_blocking(move || fetch::read_directory(Path::new(&path)))
                .await
                .expect("spawn_blocking panicked")?
        }
        MarketplaceSource::Npm { package } => fetch::fetch_npm(package, &cache_base).await?,
    };

    let install_location = match source {
        MarketplaceSource::GitHub { .. } | MarketplaceSource::Git { .. } => {
            marketplace_cache_dir(&cache_base, name)
                .map_err(MarketplaceError::InvalidName)?
                .display()
                .to_string()
        }
        MarketplaceSource::Npm { package } => npm_cache_dir(&cache_base, package)
            .map_err(|error| MarketplaceError::NpmFailed(format!("NPM 包名无效: {error}")))?
            .display()
            .to_string(),
        MarketplaceSource::Url { .. } => marketplace_cache_file(&cache_base, name)
            .map_err(MarketplaceError::InvalidName)?
            .display()
            .to_string(),
        MarketplaceSource::File { path } => path.clone(),
        MarketplaceSource::Directory { path } => path.clone(),
    };

    Ok((manifest, install_location))
}

#[cfg(test)]
#[path = "marketplace_test.rs"]
mod tests;
