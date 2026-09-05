use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};

use chrono::{DateTime, Utc};
use peri_agent::agent::async_tasks::new_tokio_command;
use tracing::warn;

use super::{
    MarketplaceError, find_marketplace_json, marketplace_cache_dir, marketplace_cache_file,
    npm_cache_dir, read_manifest_from_path, validate_npm_package,
};
use crate::atomic_file::atomic_replace;
use crate::plugin::types::MarketplaceManifest;
#[cfg(test)]
/// marketplace 既有生命周期回归测试使用的共享错误类型别名。
pub(crate) use crate::process_lifecycle::ProcessLifecycleError as ExternalCommandError;
use crate::process_lifecycle::{ProcessLifecycleError, run_short_lived_command};

/// Marketplace Git/npm 远程操作的最长执行时间，与 Tauri 来源取得入口保持一致。
const MARKETPLACE_REMOTE_TIMEOUT: Duration = Duration::from_secs(300);

/// 同一目标缓存的提升锁，避免多个调用方同时竞争最终目录名。
static CACHE_PROMOTION_LOCKS: OnceLock<
    parking_lot::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
> = OnceLock::new();

/// 保留 marketplace 测试和既有模块内调用使用的无 stdin 兼容入口。
///
/// 实际生命周期实现位于顶层共享 runner；该函数只补充 Git/npm 默认的空 stdin。
#[cfg(test)]
pub(crate) async fn run_external_command(
    command: tokio::process::Command,
    timeout: Duration,
) -> Result<std::process::Output, ExternalCommandError> {
    run_short_lived_command(command, None, timeout).await
}

/// 持有单个缓存提升锁的 RAII 租约，空闲后从全局表移除避免路径无限积累。
struct CachePromotionLockLease {
    /// 用于从锁表精确移除当前缓存的规范化键。
    key: PathBuf,
    /// 防止同一缓存目录的 clone/promotion 互相竞争。
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl CachePromotionLockLease {
    /// 获取目标缓存对应的锁租约，并用规范化父目录合并相对路径别名。
    fn new(cache_dir: &Path) -> Self {
        let parent = cache_dir
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let key = std::fs::canonicalize(parent)
            .unwrap_or_else(|_| parent.to_path_buf())
            .join(cache_dir.file_name().unwrap_or_default());
        let locks = CACHE_PROMOTION_LOCKS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()));
        let mut locks = locks.lock();
        let lock = Arc::clone(
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        );
        Self { key, lock }
    }
}

impl Drop for CachePromotionLockLease {
    /// 无等待者时移除锁表条目；仍在等待的调用方会持有自己的 Arc 并接管清理。
    fn drop(&mut self) {
        let Some(locks) = CACHE_PROMOTION_LOCKS.get() else {
            return;
        };
        let mut locks = locks.lock();
        if Arc::strong_count(&self.lock) == 2
            && locks
                .get(&self.key)
                .is_some_and(|lock| Arc::ptr_eq(lock, &self.lock))
        {
            locks.remove(&self.key);
        }
    }
}

/// 从 GitHub 仓库取得 marketplace 清单。
pub(crate) async fn fetch_github(
    name: &str,
    repo: &str,
    cache_base: &Path,
    auto_update: bool,
) -> Result<MarketplaceManifest, MarketplaceError> {
    let url = format!("https://github.com/{repo}.git");
    fetch_git(name, &url, cache_base, auto_update).await
}

/// 通用的 git 仓库（任意 git URL）
pub(crate) async fn fetch_git(
    name: &str,
    url: &str,
    cache_base: &Path,
    auto_update: bool,
) -> Result<MarketplaceManifest, MarketplaceError> {
    let cache_dir =
        marketplace_cache_dir(cache_base, name).map_err(MarketplaceError::InvalidName)?;

    if !marketplace_cache_is_valid(&cache_dir) {
        clone_git_checkout(url, &cache_dir, marketplace_cache_is_valid)
            .await
            .map_err(MarketplaceError::GitFailed)?;
    } else if auto_update {
        let mut command = new_tokio_command("git");
        command
            .args(["-C", &cache_dir.display().to_string(), "pull", "--ff-only"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "Never");
        let output = run_short_lived_command(command, None, MARKETPLACE_REMOTE_TIMEOUT)
            .await
            .map_err(|error| match error {
                ProcessLifecycleError::Timeout => MarketplaceError::GitFailed("pull 超时".into()),
                ProcessLifecycleError::Io(error) => {
                    MarketplaceError::GitFailed(format!("pull 执行失败: {error}"))
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("git pull 失败 '{}', 回退到缓存: {stderr}", url);
            // fall through to read cache
        }
    }

    let manifest_path =
        find_marketplace_json(&cache_dir).ok_or_else(|| MarketplaceError::ManifestNotFound {
            path: cache_dir.display().to_string(),
        })?;
    read_manifest_from_path(&manifest_path)
}

/// 判断 marketplace 缓存是否包含可解析的清单；空目录和损坏清单都可被重新 clone。
fn marketplace_cache_is_valid(cache_dir: &Path) -> bool {
    read_cached_manifest(cache_dir).is_some()
}

/// 读取缓存中可解析的 marketplace 清单，供并发提升完成后的复用路径使用。
fn read_cached_manifest(cache_dir: &Path) -> Option<MarketplaceManifest> {
    find_marketplace_json(cache_dir)
        .and_then(|manifest_path| read_manifest_from_path(&manifest_path).ok())
}

/// 判断外部插件缓存是否为完整的 Git checkout。
pub(crate) fn git_checkout_is_valid(cache_dir: &Path) -> bool {
    cache_dir.is_dir() && cache_dir.join(".git").exists()
}

/// 删除尚未完成的缓存条目，兼容目录、普通文件和悬空符号链接。
fn remove_invalid_cache(cache_dir: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(cache_dir) {
        Ok(metadata) if metadata.is_dir() => {
            std::fs::remove_dir_all(cache_dir)
                .map_err(|error| format!("删除无效缓存失败: {error}"))?;
        }
        Ok(_) => {
            std::fs::remove_file(cache_dir)
                .map_err(|error| format!("删除无效缓存失败: {error}"))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("检查 Git 缓存失败: {error}")),
    }
    Ok(())
}

/// 在目标父目录中完成临时 Git checkout，并将完整结果原子提升到正式缓存。
///
/// `cache_is_valid` 同时用于清理旧的无效缓存和处理其他进程已完成的并发提升；
/// 因此 clone 失败、校验失败或目标竞争失败时，不会把临时目录误留在正式路径。
pub(crate) async fn clone_git_checkout(
    url: &str,
    cache_dir: &Path,
    cache_is_valid: fn(&Path) -> bool,
) -> Result<(), String> {
    let parent = cache_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| format!("创建 Git 缓存父目录失败: {error}"))?;

    let lease = CachePromotionLockLease::new(cache_dir);
    let _guard = lease.lock.lock().await;

    // 等待锁期间其他调用方可能已经完成提升，直接复用其完整缓存。
    if cache_is_valid(cache_dir) {
        return Ok(());
    }
    remove_invalid_cache(cache_dir)?;

    let temporary = tempfile::Builder::new()
        .prefix("peri-git-clone-")
        .tempdir_in(parent)
        .map_err(|error| format!("创建 Git 临时目录失败: {error}"))?;
    let checkout_dir = temporary.path().join("checkout");
    let checkout_path = checkout_dir.to_string_lossy().into_owned();
    let mut command = new_tokio_command("git");
    command
        .args(["clone", "--depth", "1", "--", url, &checkout_path])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never");
    let output = run_short_lived_command(command, None, MARKETPLACE_REMOTE_TIMEOUT)
        .await
        .map_err(|error| match error {
            ProcessLifecycleError::Timeout => "clone 超时".to_owned(),
            ProcessLifecycleError::Io(error) => format!("clone 执行失败: {error}"),
        })?;
    if !output.status.success() {
        return Err(format!(
            "clone 失败: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    if !cache_is_valid(&checkout_dir) {
        return Err("git clone 产生的 checkout 无效".into());
    }

    // 其他进程可能不受进程内锁约束；若它已提交完整缓存，当前结果无需覆盖。
    if cache_is_valid(cache_dir) {
        return Ok(());
    }
    match std::fs::rename(&checkout_dir, cache_dir) {
        Ok(()) => Ok(()),
        Err(error)
            if error.kind() == std::io::ErrorKind::AlreadyExists && cache_is_valid(cache_dir) =>
        {
            Ok(())
        }
        Err(error) => Err(format!("提交 Git 缓存失败: {error}")),
    }
}

pub(crate) async fn fetch_url(
    name: &str,
    url: &str,
    cache_base: &Path,
) -> Result<MarketplaceManifest, MarketplaceError> {
    let cache_file =
        marketplace_cache_file(cache_base, name).map_err(MarketplaceError::InvalidName)?;

    let last_modified = std::fs::metadata(&cache_file)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let dt: DateTime<Utc> = t.into();
            dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
        });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| MarketplaceError::HttpFailed(e.to_string()))?;

    let mut req = client.get(url);
    if let Some(ref lm) = last_modified {
        req = req.header("If-Modified-Since", lm);
    }

    let result = req.send().await;

    match result {
        Ok(response) => match response.status().as_u16() {
            304 => read_manifest_from_path(&cache_file),
            200 => {
                let body = response
                    .text()
                    .await
                    .map_err(|e| MarketplaceError::HttpFailed(e.to_string()))?;
                if let Some(parent) = cache_file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                atomic_replace(&cache_file, body.as_bytes())
                    .map_err(|error| MarketplaceError::Io(error.into_io_error()))?;
                serde_json::from_str(&body)
                    .map_err(|e| MarketplaceError::ParseFailed(e.to_string()))
            }
            status => Err(MarketplaceError::HttpFailed(format!("HTTP {status}"))),
        },
        Err(e) => {
            if cache_file.exists() {
                warn!("URL 拉取失败 '{}', 回退到缓存: {}", url, e);
                read_manifest_from_path(&cache_file)
            } else {
                Err(MarketplaceError::HttpFailed(e.to_string()))
            }
        }
    }
}

pub(crate) fn read_file(path: &Path) -> Result<MarketplaceManifest, MarketplaceError> {
    read_manifest_from_path(path)
}

pub(crate) fn read_directory(path: &Path) -> Result<MarketplaceManifest, MarketplaceError> {
    let manifest_path =
        find_marketplace_json(path).ok_or_else(|| MarketplaceError::ManifestNotFound {
            path: path.display().to_string(),
        })?;
    read_manifest_from_path(&manifest_path)
}

/// 通过 npm 直接取得并解包 marketplace 清单。
pub(crate) async fn fetch_npm(
    package: &str,
    cache_base: &Path,
) -> Result<MarketplaceManifest, MarketplaceError> {
    validate_npm_package(package)
        .map_err(|error| MarketplaceError::NpmFailed(format!("NPM 包名无效: {error}")))?;
    let cache_dir = npm_cache_dir(cache_base, package)
        .map_err(|error| MarketplaceError::NpmFailed(format!("NPM 包名无效: {error}")))?;

    std::fs::create_dir_all(cache_base)?;
    let lease = CachePromotionLockLease::new(&cache_dir);
    let _guard = lease.lock.lock().await;

    if let Some(manifest) = read_cached_manifest(&cache_dir) {
        return Ok(manifest);
    }
    // 损坏或中断留下的正式目录不能阻止下一次 npm pack 提升。
    remove_invalid_cache(&cache_dir).map_err(MarketplaceError::NpmFailed)?;

    // 临时目录放在缓存根同一文件系统中，随后将标准 npm pack 的 package/
    // 根目录提升为 cache_dir，避免留下 cache_dir/package 的错误布局。
    let temporary = tempfile::Builder::new()
        .prefix("peri-npm-pack-")
        .tempdir_in(cache_base)
        .map_err(|error| MarketplaceError::NpmFailed(format!("创建 npm 临时目录失败: {error}")))?;
    let temporary_path = temporary.path().display().to_string();
    let mut command = new_tokio_command("npm");
    command.args([
        "pack",
        "--ignore-scripts",
        "--pack-destination",
        temporary_path.as_str(),
        "--",
        package,
    ]);
    let output = run_short_lived_command(command, None, MARKETPLACE_REMOTE_TIMEOUT)
        .await
        .map_err(|error| match error {
            ProcessLifecycleError::Timeout => MarketplaceError::NpmFailed("npm pack 超时".into()),
            ProcessLifecycleError::Io(error) => {
                MarketplaceError::NpmFailed(format!("npm pack 执行失败: {error}"))
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(MarketplaceError::NpmFailed(format!(
            "npm pack 失败: {stderr}"
        )));
    }

    let tgz_path = std::fs::read_dir(temporary.path())?
        .find_map(|e| {
            e.ok().and_then(|f| {
                if f.path()
                    .extension()
                    .map(|ext| ext == "tgz")
                    .unwrap_or(false)
                {
                    Some(f.path())
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| MarketplaceError::NpmFailed("未找到 .tgz 文件".into()))?;

    let file = std::fs::File::open(&tgz_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(temporary.path())?;

    let package_root = temporary.path().join("package");
    let manifest_path =
        find_marketplace_json(&package_root).ok_or_else(|| MarketplaceError::ManifestNotFound {
            path: package_root.display().to_string(),
        })?;
    let manifest = read_manifest_from_path(&manifest_path)?;

    // package_root 与 cache_dir 同处 cache_base，rename 在 Windows 上也不会
    // 跨卷；manifest 在提升前已解析，失败时不会留下半成品缓存。
    if let Some(existing) = read_cached_manifest(&cache_dir) {
        return Ok(existing);
    }
    match std::fs::rename(&package_root, &cache_dir) {
        Ok(()) => Ok(manifest),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            read_cached_manifest(&cache_dir).ok_or(MarketplaceError::Io(error))
        }
        Err(error) => Err(MarketplaceError::Io(error)),
    }
}
