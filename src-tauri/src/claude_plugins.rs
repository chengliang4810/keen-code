//! Claude Code 插件兼容层。
//!
//! 本模块只处理 Claude Code 的插件市场/插件清单、可审计的来源解析以及运行时投影；
//! 不依赖 Tauri，也不直接执行网络、Git、npm 或 pip 命令。调用方可以把
//! [`SourceFetchPlan`] 交给受审计的系统能力层执行，再调用本模块安装和加载。
//! 这样 extensions 与 peri runtime 可以复用完全相同的解析、依赖和变量规则。

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

use crate::http_response::{HttpResponseReadError, read_http_response_limited};
use crate::path_utils::{is_safe_relative_path, path_to_frontend};

mod model;
pub use model::*;

/// 单个 MCPB/DXT 归档允许读取的最大字节数，避免把插件包当作无限制下载器。
const MAX_MCPB_BYTES: usize = 128 * 1024 * 1024;
/// MCPB/DXT 解包后的总文件大小上限，防止 ZIP 炸弹耗尽磁盘。
const MAX_MCPB_EXTRACTED_BYTES: u64 = 256 * 1024 * 1024;
/// 单个 MCPB/DXT 归档允许包含的最大文件数量。
const MAX_MCPB_ENTRIES: usize = 4096;
/// 只有解包完成后才会写入的内容缓存完成标记。
const MCPB_COMPLETION_MARKER: &str = ".keencode-mcpb-complete";
/// 解包提交只占用短临界区；远程下载在加锁前完成，避免并发清理同一内容目录。
static MCPB_EXTRACTION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 创建受控插件根目录。根目录之前的平台祖先（例如 macOS `/var`）不属于插件
/// 数据边界，只检查直接承载 `claude-plugins` 的父级和根本身。
fn ensure_controlled_root(path: &Path, label: &str) -> Result<()> {
    reject_parent_components(path, label)?;
    if let Some(parent) = path.parent() {
        validate_ancestor_chain(parent, label)?;
    }
    validate_directory_boundary(path, label)?;
    let parent = path
        .parent()
        .ok_or_else(|| ClaudePluginError::Invalid(format!("{label}路径缺少父目录")))?;
    fs::create_dir_all(parent)?;
    ensure_one_directory(path, label)
}

fn validate_controlled_root(path: &Path, label: &str) -> Result<()> {
    reject_parent_components(path, label)?;
    if let Some(parent) = path.parent() {
        validate_ancestor_chain(parent, label)?;
    }
    validate_directory_boundary(path, label)
}

/// 从已验证的受控目录边界开始逐层创建子目录，不跟随任何受控层符号链接。
fn ensure_controlled_descendant_chain(boundary: &Path, path: &Path, label: &str) -> Result<()> {
    reject_parent_components(path, label)?;
    validate_directory_boundary(boundary, label)?;
    let relative = path.strip_prefix(boundary).map_err(|_| {
        ClaudePluginError::Invalid(format!("{label}路径不在受控目录边界内：{}", path.display()))
    })?;
    let mut current = boundary.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(ClaudePluginError::Invalid(format!(
                "{label}路径包含非法目录层：{}",
                path.display()
            )));
        };
        current.push(name);
        ensure_one_directory(&current, label)?;
    }
    Ok(())
}

/// 检查受控边界以内已经存在的路径层级，允许末端文件；缺失末端由调用方决定
/// 是返回空状态还是创建目录。
fn validate_controlled_path(boundary: &Path, path: &Path, label: &str) -> Result<()> {
    reject_parent_components(path, label)?;
    validate_directory_boundary(boundary, label)?;
    let relative = path.strip_prefix(boundary).map_err(|_| {
        ClaudePluginError::Invalid(format!("{label}路径不在受控目录边界内：{}", path.display()))
    })?;
    let mut current = boundary.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(ClaudePluginError::Invalid(format!(
                "{label}路径包含非法目录层：{}",
                path.display()
            )));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ClaudePluginError::Invalid(format!(
                    "{label}路径不允许跟随符号链接：{}",
                    current.display()
                )));
            }
            Ok(metadata) if current != path && !metadata.is_dir() => {
                return Err(ClaudePluginError::Invalid(format!(
                    "{label}路径的父级不是目录：{}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_directory_boundary(path: &Path, label: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && let Ok(metadata) = fs::symlink_metadata(parent)
        && metadata.file_type().is_symlink()
    {
        return Err(ClaudePluginError::Invalid(format!(
            "{label}路径的直接父级不允许是符号链接：{}",
            parent.display()
        )));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(ClaudePluginError::Invalid(
            format!("{label}路径不允许是符号链接：{}", path.display()),
        )),
        Ok(metadata) if !metadata.is_dir() => Err(ClaudePluginError::Invalid(format!(
            "{label}路径不是目录：{}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// 检查受控根目录之前的完整既有父级链。macOS 的 `/var`、`/tmp` 是系统固定
/// 别名，允许它们本身的符号链接；其下任何用户数据组件的符号链接仍然拒绝。
fn validate_ancestor_chain(path: &Path, label: &str) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
            Component::CurDir => continue,
            Component::ParentDir => return reject_parent_components(path, label),
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if !is_allowed_platform_alias(&current) {
                    return Err(ClaudePluginError::Invalid(format!(
                        "{label}路径的父级不允许是符号链接：{}",
                        current.display()
                    )));
                }
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(ClaudePluginError::Invalid(format!(
                    "{label}路径的父级不是目录：{}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn is_allowed_platform_alias(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        return path == Path::new("/var") || path == Path::new("/tmp");
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

fn ensure_one_directory(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(ClaudePluginError::Invalid(format!(
                "{label}路径不允许是符号链接：{}",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(ClaudePluginError::Invalid(format!(
                "{label}路径不是目录：{}",
                path.display()
            )));
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(ClaudePluginError::Invalid(format!(
                "创建{label}失败：{}：{error}",
                path.display()
            )));
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "创建后的{label}路径不是普通目录：{}",
            path.display()
        )));
    }
    Ok(())
}

fn reject_parent_components(path: &Path, label: &str) -> Result<()> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ClaudePluginError::Invalid(format!(
            "{label}路径不允许包含父目录：{}",
            path.display()
        )));
    }
    Ok(())
}

/// 已安装插件的公开记录；绝不包含敏感配置值。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    /// 唯一的 `plugin@marketplace` 标识。
    pub id: PluginId,
    /// 已缓存的插件版本。
    pub version: String,
    /// 版本化缓存根目录。
    pub install_path: PathBuf,
    /// 是否参与运行时快照。
    pub enabled: bool,
    /// 用户配置中非敏感字段的 JSON 值。
    #[serde(default)]
    pub public_user_config: BTreeMap<String, Value>,
    /// 已写入安全存储的敏感字段名，不包含值。
    #[serde(default)]
    pub sensitive_user_config_keys: BTreeSet<String>,
    /// 当前插件敏感配置所在的安全存储代际；只作为公开指针，不是敏感值。
    #[serde(default)]
    pub secret_generation: u64,
}

/// 非敏感状态文件内容。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginState {
    /// 安装插件的公开状态。
    #[serde(default)]
    pub plugins: Vec<InstalledPlugin>,
}

/// 密钥库抽象；extensions 可接 Tauri Stronghold/系统钥匙串，测试可用内存实现。
pub trait SecretStore {
    /// 写入敏感 JSON 值。
    fn set_json(&mut self, key: &str, value: &Value) -> Result<()>;
    /// 读取敏感 JSON 值。
    fn get_json(&self, key: &str) -> Result<Option<Value>>;
    /// 删除敏感 JSON 值。
    fn delete(&mut self, key: &str) -> Result<()>;
}

/// 只用于单元测试和调用方适配测试的内存安全存储，不应替代生产密钥库。
#[cfg(test)]
#[derive(Debug, Default)]
struct InMemorySecretStore {
    /// 进程内测试数据。
    values: BTreeMap<String, Value>,
}

#[cfg(test)]
impl SecretStore for InMemorySecretStore {
    /// 写入进程内测试值。
    fn set_json(&mut self, key: &str, value: &Value) -> Result<()> {
        self.values.insert(key.to_owned(), value.clone());
        Ok(())
    }

    /// 读取进程内测试值。
    fn get_json(&self, key: &str) -> Result<Option<Value>> {
        Ok(self.values.get(key).cloned())
    }

    /// 删除进程内测试值。
    fn delete(&mut self, key: &str) -> Result<()> {
        self.values.remove(key);
        Ok(())
    }
}

/// 安装来源与已经验证的插件目录。
#[derive(Clone, Debug)]
pub struct MaterializedPlugin {
    /// 市场清单中的唯一插件 ID。
    pub id: PluginId,
    /// 来源下载、解包或检出后得到的插件根目录。
    pub source_root: PathBuf,
}

/// 对公开状态和安全存储执行的一次用户配置更新。
#[derive(Clone, Debug, Default)]
pub struct UserConfigUpdate {
    /// 传入的新值，敏感字段也在此处出现但不会写入公开状态。
    pub values: BTreeMap<String, Value>,
    /// 是否删除未在本次输入中出现的旧值。
    pub replace: bool,
}

/// 已经完成校验、等待提交的敏感配置变更；此阶段不触碰 SecretStore。
#[derive(Debug)]
struct PlannedSecretChange {
    /// 由插件 ID 和配置字段生成的安全存储键。
    key: String,
    /// `Some` 表示写入新值，`None` 表示删除旧值。
    value: Option<Value>,
}

/// 应用敏感配置前读取的旧值，用于失败补偿。
#[derive(Debug)]
struct SecretUndo {
    /// 与待应用变更对应的安全存储键。
    key: String,
    /// 旧值不存在时回滚为删除。
    value: Option<Value>,
}

/// 已经完成公开状态计算和敏感变更规划的用户配置事务。
#[derive(Debug)]
struct UserConfigPlan {
    /// 新的非敏感配置公开值。
    public_user_config: BTreeMap<String, Value>,
    /// 新的敏感配置字段名集合。
    sensitive_user_config_keys: BTreeSet<String>,
    /// 等待提交的敏感配置操作。
    secret_changes: Vec<PlannedSecretChange>,
}

/// 将 SecretStore 的错误转换为不包含密钥名或密钥值的事务错误。
fn secret_transaction_error(id: &PluginId, action: &str) -> ClaudePluginError {
    ClaudePluginError::Invalid(format!("插件 {id} 密钥事务{action}"))
}

/// 在任何密钥写入前读取所有待变更键的旧值。
fn capture_secret_undo(
    id: &PluginId,
    changes: &[PlannedSecretChange],
    secrets: &dyn SecretStore,
) -> Result<Vec<SecretUndo>> {
    let mut undo = Vec::with_capacity(changes.len());
    for change in changes {
        let value = secrets
            .get_json(&change.key)
            .map_err(|_| secret_transaction_error(id, "读取旧值失败，未写入密钥"))?;
        undo.push(SecretUndo {
            key: change.key.clone(),
            value,
        });
    }
    Ok(undo)
}

/// 尽力把一组已应用的敏感配置恢复到旧值；错误只返回失败项数量。
fn rollback_secret_changes(secrets: &mut dyn SecretStore, undo: &[SecretUndo]) -> usize {
    let mut failures = 0;
    for change in undo.iter().rev() {
        let result = match &change.value {
            Some(value) => secrets.set_json(&change.key, value),
            None => secrets.delete(&change.key),
        };
        if result.is_err() {
            failures += 1;
        }
    }
    failures
}

/// 应用敏感配置；任一操作失败都补偿本次已尝试的操作。
fn apply_secret_changes(
    id: &PluginId,
    changes: &[PlannedSecretChange],
    undo: &[SecretUndo],
    secrets: &mut dyn SecretStore,
) -> Result<()> {
    debug_assert_eq!(changes.len(), undo.len());
    for (index, change) in changes.iter().enumerate() {
        let result = match &change.value {
            Some(value) => secrets.set_json(&change.key, value),
            None => secrets.delete(&change.key),
        };
        if result.is_err() {
            let rollback_failures = rollback_secret_changes(secrets, &undo[..=index]);
            return Err(if rollback_failures == 0 {
                secret_transaction_error(id, "应用变更失败，已回滚密钥")
            } else {
                secret_transaction_error(
                    id,
                    &format!("应用变更失败，密钥回滚失败（{rollback_failures} 项）"),
                )
            });
        }
    }
    Ok(())
}

/// 在公开 state 已经提交后清理不再被它引用的安全存储代际。清理失败只留下
/// 不可被当前状态读取的孤儿密钥，不能回滚或删除仍被 state 指向的旧代际。
fn cleanup_secret_generation(
    storage: &PluginStorage,
    id: &PluginId,
    generation: u64,
    names: &BTreeSet<String>,
    secrets: &mut dyn SecretStore,
) {
    for name in names {
        let key = match storage.secret_key_at(id, name, generation) {
            Ok(key) => key,
            Err(_) => {
                tracing::warn!(plugin = %id, generation, "清理 Claude 插件旧代际密钥键名失败");
                continue;
            }
        };
        if secrets.delete(&key).is_err() {
            tracing::warn!(plugin = %id, generation, "清理 Claude 插件旧代际密钥失败");
        }
    }
}

/// 安全安装、状态读写和运行时快照的纯 Rust 服务。
#[derive(Clone, Debug)]
pub struct ClaudePluginManager {
    /// 当前唯一的缓存和状态布局。
    pub storage: PluginStorage,
}

impl ClaudePluginManager {
    /// 根据应用数据根创建服务。
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            storage: PluginStorage::under(data_root),
        }
    }

    /// 从公开 JSON 状态文件读取安装记录；文件不存在时返回空状态。
    pub fn load_state(&self) -> Result<PluginState> {
        self.storage.validate_layout()?;
        match fs::symlink_metadata(&self.storage.state_path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(PluginState::default());
            }
            Err(error) => return Err(error.into()),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ClaudePluginError::Invalid(format!(
                    "不允许读取符号链接插件状态文件：{}",
                    self.storage.state_path.display()
                )));
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件状态文件不是普通文件：{}",
                    self.storage.state_path.display()
                )));
            }
            Ok(_) => {}
        }
        let state: PluginState = match serde_json::from_slice(&read_limited(
            &self.storage.state_path,
        )?) {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(path = %self.storage.state_path.display(), %error, "插件状态无效，本次按空状态继续");
                return Ok(PluginState::default());
            }
        };
        if let Err(error) = validate_state(&self.storage, &state) {
            tracing::warn!(path = %self.storage.state_path.display(), %error, "插件状态校验失败，本次按空状态继续");
            return Ok(PluginState::default());
        }
        Ok(state)
    }

    /// 原子写入公开状态，确保敏感值不进入 state.json。
    pub fn save_state(&self, state: &PluginState) -> Result<()> {
        self.storage.ensure_directories()?;
        validate_state(&self.storage, state)?;
        if let Ok(metadata) = fs::symlink_metadata(&self.storage.state_path)
            && (metadata.file_type().is_symlink() || !metadata.is_file())
        {
            return Err(ClaudePluginError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "状态目标不是可替换的普通文件：{}",
                    self.storage.state_path.display()
                ),
            )));
        }
        let bytes = serde_json::to_vec_pretty(state)?;
        crate::storage::atomic_write_private(&self.storage.state_path, &bytes)
            .map_err(|error| ClaudePluginError::Io(io::Error::other(error)))?;
        Ok(())
    }

    /// 复制验证过的插件目录到内容指纹缓存；用户配置需通过单独配置接口保存。
    #[cfg(test)]
    fn install_from_directory(
        &self,
        materialized: MaterializedPlugin,
        config: UserConfigUpdate,
        secrets: &mut dyn SecretStore,
    ) -> Result<()> {
        self.install_from_directories(vec![materialized], config, secrets)
    }

    /// 预解析并批量安装一组插件。
    ///
    /// 调用方应按依赖在前的拓扑顺序传入插件。所有清单、ID 和来源目录都会在
    /// 修改公开状态前完成校验；缓存副本和公开状态只在整批准备成功后提交。
    /// 当前生产调用只传入空的 UserConfigUpdate；批量接口拒绝配置写入，避免
    /// 在没有 SecretStore 事务能力时虚假承诺“状态、缓存和密钥”完全原子。
    pub fn install_from_directories(
        &self,
        materialized: Vec<MaterializedPlugin>,
        config: UserConfigUpdate,
        secrets: &mut dyn SecretStore,
    ) -> Result<()> {
        if materialized.is_empty() {
            return Err(ClaudePluginError::Invalid(
                "插件安装计划不能为空".to_owned(),
            ));
        }
        if config.replace || !config.values.is_empty() {
            return Err(ClaudePluginError::Invalid(
                "批量插件安装不接受 userConfig；请安装后单独保存插件配置".to_owned(),
            ));
        }

        #[derive(Debug)]
        struct PreparedInstallation {
            id: PluginId,
            manifest: PluginManifest,
            source_root: PathBuf,
            destination: PathBuf,
        }

        let mut prepared = Vec::with_capacity(materialized.len());
        let mut ids = BTreeSet::new();
        for materialized in materialized {
            let id = require_marketplace_id(&materialized.id)?;
            let id_key = format!(
                "{}@{}",
                marketplace_name_key(id.marketplace.as_deref().unwrap_or_default()),
                marketplace_name_key(&id.plugin)
            );
            if !ids.insert(id_key) {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件安装计划包含重复 ID：{id}"
                )));
            }
            let source_root = canonical_plugin_root(&materialized.source_root)?;
            let manifest = load_plugin_manifest(&source_root)?;
            if !manifest.name.eq_ignore_ascii_case(&id.plugin) {
                return Err(ClaudePluginError::Invalid(format!(
                    "市场插件 ID {} 与 plugin.json name {} 不一致",
                    id, manifest.name
                )));
            }
            let content_fingerprint = plugin_tree_fingerprint(&source_root)?;
            // 缓存目录使用来源内容指纹，而不是仅使用 manifest.version。远程来源
            // 即使没有提升版本号，代码变化也必须得到新的缓存副本。
            let destination = self.storage.versioned_path(&id, &content_fingerprint)?;
            prepared.push(PreparedInstallation {
                id,
                manifest,
                source_root,
                destination,
            });
        }

        // 先完成所有用户配置字段校验，避免批量复制或写入密钥后才发现
        // 某个依赖的配置无效。
        for installation in &prepared {
            for (name, value) in &config.values {
                let definition = installation.manifest.user_config.get(name).ok_or_else(|| {
                    ClaudePluginError::Invalid(format!(
                        "插件 {} 没有 userConfig 字段 {name}",
                        installation.id
                    ))
                })?;
                validate_user_config_value(name, definition, value)?;
            }
        }

        // 读取旧状态并准备完整的新状态；此处不写磁盘。
        let previous_state = self.load_state()?;
        let mut next_state = previous_state.clone();
        let mut copied_destinations = Vec::new();
        self.storage.ensure_directories()?;
        for installation in &prepared {
            if !installation.destination.exists() {
                if let Err(error) =
                    copy_plugin_tree(&installation.source_root, &installation.destination)
                {
                    cleanup_copied_plugins(&copied_destinations);
                    return Err(error);
                }
                copied_destinations.push(installation.destination.clone());
            }
        }

        let mut removed_secret_generations = Vec::new();
        for installation in prepared {
            let index = next_state
                .plugins
                .iter()
                .position(|item| plugin_ids_equal_ascii_case(&item.id, &installation.id));
            let previous = index.map(|index| next_state.plugins.remove(index));
            let previous_secret_generation =
                previous.as_ref().map_or(0, |item| item.secret_generation);
            let (public_user_config, sensitive_user_config_keys) = match self.apply_user_config(
                &installation.id,
                &installation.manifest,
                previous.as_ref(),
                config.clone(),
                secrets,
                false,
            ) {
                Ok(config) => config,
                Err(error) => {
                    cleanup_copied_plugins(&copied_destinations);
                    return Err(error);
                }
            };
            let mut public_user_config = public_user_config;
            let mut sensitive_user_config_keys = sensitive_user_config_keys;
            let mut removed_sensitive_keys = BTreeSet::new();
            if previous.is_some() {
                // 安装更新不接受新的 userConfig 值，但清单本身可能新增、删除或
                // 改变字段的 sensitive 属性。公开状态只保留当前清单仍声明的
                // 同类型字段；敏感字段状态只保留确实仍写入 SecretStore 的键名。
                public_user_config.retain(|name, _| {
                    installation
                        .manifest
                        .user_config
                        .get(name)
                        .is_some_and(|definition| !definition.sensitive)
                });
                sensitive_user_config_keys.retain(|name| {
                    let keep = installation
                        .manifest
                        .user_config
                        .get(name)
                        .is_some_and(|definition| definition.sensitive);
                    if !keep {
                        removed_sensitive_keys.insert(name.clone());
                    }
                    keep
                });
                // public -> sensitive 的清单变更不能把旧公开值继续留在 state；
                // 需要用户在新的敏感配置入口中重新写入 SecretStore。
                for name in installation.manifest.user_config.keys() {
                    if installation
                        .manifest
                        .user_config
                        .get(name)
                        .is_some_and(|definition| definition.sensitive)
                    {
                        public_user_config.remove(name);
                    }
                }
            }
            // 安装阶段允许先落盘再配置 required userConfig；未完成配置的插件保持禁用，
            // 避免安装命令因为运行时插值缺失而失败，同时让设置页可以补齐配置后启用。
            let enabled = previous.as_ref().is_none_or(|item| item.enabled)
                && has_complete_required_user_config(
                    &installation.manifest,
                    &public_user_config,
                    &sensitive_user_config_keys,
                );
            if !removed_sensitive_keys.is_empty() {
                removed_secret_generations.push((
                    installation.id.clone(),
                    previous_secret_generation,
                    removed_sensitive_keys,
                ));
            }
            next_state.plugins.push(InstalledPlugin {
                id: installation.id,
                version: installation
                    .manifest
                    .version
                    .as_deref()
                    .unwrap_or("unversioned")
                    .to_owned(),
                install_path: installation.destination,
                enabled,
                public_user_config,
                sensitive_user_config_keys,
                secret_generation: previous_secret_generation,
            });
        }
        next_state
            .plugins
            .sort_by(|left, right| left.id.cmp(&right.id));
        if let Err(error) = self.save_state(&next_state) {
            cleanup_copied_plugins(&copied_destinations);
            return Err(error);
        }
        for (id, generation, names) in removed_secret_generations {
            cleanup_secret_generation(&self.storage, &id, generation, &names, secrets);
        }
        cleanup_unreferenced_plugin_caches(&self.storage, &previous_state, &next_state);
        Ok(())
    }

    /// 写入用户配置时把 sensitive 值交给密钥库，公开状态只记录字段名。
    pub fn update_user_config(
        &self,
        id: &PluginId,
        config: UserConfigUpdate,
        secrets: &mut dyn SecretStore,
    ) -> Result<()> {
        let id = require_marketplace_id(id)?;
        let mut state = self.load_state()?;
        let index = state
            .plugins
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| ClaudePluginError::Invalid(format!("没有安装插件：{id}")))?;
        let previous = state.plugins[index].clone();
        let manifest = load_plugin_manifest(&previous.install_path)?;
        let config_values = config.values.clone();
        let UserConfigPlan {
            public_user_config,
            sensitive_user_config_keys,
            secret_changes,
        } = self.plan_user_config(&id, &manifest, Some(&previous), config, true)?;
        let config_has_sensitive_change =
            secret_changes.iter().any(|change| change.value.is_some())
                || secret_changes.iter().any(|change| change.value.is_none());
        let installed = InstalledPlugin {
            public_user_config,
            sensitive_user_config_keys,
            ..previous
        };
        let mut installed = installed;

        if config_has_sensitive_change {
            // 敏感配置使用每个插件独立的代际槽：先完整写入非活动代际，再把
            // `secret_generation` 随公开 state 一起原子切换。旧代际在提交后才清理，
            // 因而任意崩溃点都至少保留一个 state 可读取的完整版本。
            let next_generation = previous
                .secret_generation
                .checked_add(1)
                .ok_or_else(|| secret_transaction_error(&id, "代际编号耗尽，未写入密钥"))?;
            let mut generation_changes = Vec::new();
            let names = previous
                .sensitive_user_config_keys
                .union(&installed.sensitive_user_config_keys)
                .cloned()
                .collect::<BTreeSet<_>>();
            for name in &names {
                let key = self.storage.secret_key_at(&id, name, next_generation)?;
                if installed.sensitive_user_config_keys.contains(name) {
                    if let Some(value) = config_values.get(name) {
                        generation_changes.push(PlannedSecretChange {
                            key,
                            value: Some(value.clone()),
                        });
                    } else if previous.sensitive_user_config_keys.contains(name) {
                        let old_key =
                            self.storage
                                .secret_key_at(&id, name, previous.secret_generation)?;
                        let value = secrets
                            .get_json(&old_key)
                            .map_err(|_| {
                                secret_transaction_error(
                                    &id,
                                    "读取旧代际密钥失败，未写入新代际密钥",
                                )
                            })?
                            .ok_or_else(|| {
                                secret_transaction_error(&id, "旧代际密钥缺失，未写入新代际密钥")
                            })?;
                        generation_changes.push(PlannedSecretChange {
                            key,
                            value: Some(value),
                        });
                    } else {
                        return Err(secret_transaction_error(&id, "新敏感配置没有可写入的值"));
                    }
                } else {
                    // 删除新代际中可能残留的同名孤儿，避免重用代际编号时把旧
                    // 敏感值带入新的公开状态。
                    generation_changes.push(PlannedSecretChange { key, value: None });
                }
            }
            installed.secret_generation = next_generation;
            state.plugins[index] = installed.clone();
            validate_state(&self.storage, &state)?;
            let undo = capture_secret_undo(&id, &generation_changes, secrets)?;
            apply_secret_changes(&id, &generation_changes, &undo, secrets)?;
            if let Err(error) = self.save_state(&state) {
                let rollback_failures = rollback_secret_changes(secrets, &undo);
                cleanup_secret_generation(&self.storage, &id, next_generation, &names, secrets);
                return Err(if rollback_failures == 0 {
                    ClaudePluginError::Invalid(format!(
                        "插件 {id} 公开状态保存失败，已回滚新代际密钥：{error}"
                    ))
                } else {
                    ClaudePluginError::Invalid(format!(
                        "插件 {id} 公开状态保存失败，新代际密钥回滚失败（{rollback_failures} 项）：{error}"
                    ))
                });
            }
            cleanup_secret_generation(
                &self.storage,
                &id,
                previous.secret_generation,
                &previous.sensitive_user_config_keys,
                secrets,
            );
        } else {
            state.plugins[index] = installed.clone();
            validate_state(&self.storage, &state)?;
            self.save_state(&state)?;
        }
        Ok(())
    }

    /// 先提交删除后的公开状态，再清理插件敏感字段；缓存保留给调用方按版本回收。
    pub fn uninstall(
        &self,
        id: &PluginId,
        secrets: &mut dyn SecretStore,
    ) -> Result<InstalledPlugin> {
        let id = require_marketplace_id(id)?;
        let mut state = self.load_state()?;
        let index = state
            .plugins
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| ClaudePluginError::Invalid(format!("没有安装插件：{id}")))?;
        let removed = state.plugins.remove(index);
        validate_state(&self.storage, &state)?;
        // 仅读取待清理键，确保系统密钥库可访问；不删除、不修改任何敏感值。
        // 公开 state 仍然在此之后先提交，读取失败时原状态保持不变。
        for name in &removed.sensitive_user_config_keys {
            let key = self
                .storage
                .secret_key_at(&id, name, removed.secret_generation)?;
            secrets
                .get_json(&key)
                .map_err(|_| secret_transaction_error(&id, "读取待清理密钥失败，未提交卸载状态"))?;
        }
        // 删除事务的顺序与更新相反：state 先原子提交为“不再引用该插件”，
        // 成功后才删密钥。这样即使密钥库删除途中崩溃，也只会留下安全孤儿，
        // 不会产生 state 引用已删除密钥的状态。
        self.save_state(&state)?;
        cleanup_secret_generation(
            &self.storage,
            &id,
            removed.secret_generation,
            &removed.sensitive_user_config_keys,
            secrets,
        );
        Ok(removed)
    }

    /// 原子切换插件启用状态；缓存仍保留，后续运行时快照立即可见。
    pub fn set_enabled(&self, id: &PluginId, enabled: bool) -> Result<InstalledPlugin> {
        let id = require_marketplace_id(id)?;
        let mut state = self.load_state()?;
        let item = state
            .plugins
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| ClaudePluginError::Invalid(format!("没有安装插件：{id}")))?;
        item.enabled = enabled;
        let updated = item.clone();
        self.save_state(&state)?;
        Ok(updated)
    }

    /// 为所有启用插件构造可交给 extensions/peri runtime 的只读投影。
    pub fn runtime_snapshot(
        &self,
        project_dir: &Path,
        environment: &BTreeMap<String, String>,
        secrets: &dyn SecretStore,
    ) -> Result<PluginRuntimeSnapshot> {
        let state = self.load_state()?;
        let mut plugins = Vec::new();
        for installed in state.plugins.iter().filter(|item| item.enabled) {
            let manifest = load_plugin_manifest(&installed.install_path)?;
            let config = resolved_user_config(&self.storage, installed, &manifest, secrets)?;
            plugins.push(extract_components(
                installed.id.clone(),
                &installed.install_path,
                &manifest,
                project_dir,
                environment,
                &config,
            )?);
        }
        plugins.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(PluginRuntimeSnapshot {
            plugins,
            plugin_hooks: Vec::new(),
        })
    }

    /// 规划并应用一个用户配置变更，不会将敏感值置入返回的公开 map。
    fn apply_user_config(
        &self,
        id: &PluginId,
        manifest: &PluginManifest,
        previous: Option<&InstalledPlugin>,
        update: UserConfigUpdate,
        secrets: &mut dyn SecretStore,
        require_complete: bool,
    ) -> Result<(BTreeMap<String, Value>, BTreeSet<String>)> {
        let plan = self.plan_user_config(id, manifest, previous, update, require_complete)?;
        let undo = capture_secret_undo(id, &plan.secret_changes, secrets)?;
        apply_secret_changes(id, &plan.secret_changes, &undo, secrets)?;
        Ok((plan.public_user_config, plan.sensitive_user_config_keys))
    }

    /// 只计算公开状态和敏感操作；所有字段、类型和必填约束均在此阶段完成。
    fn plan_user_config(
        &self,
        id: &PluginId,
        manifest: &PluginManifest,
        previous: Option<&InstalledPlugin>,
        update: UserConfigUpdate,
        require_complete: bool,
    ) -> Result<UserConfigPlan> {
        let mut public = if update.replace {
            BTreeMap::new()
        } else {
            previous
                .map(|item| item.public_user_config.clone())
                .unwrap_or_default()
        };
        let mut sensitive = if update.replace {
            BTreeSet::new()
        } else {
            previous
                .map(|item| item.sensitive_user_config_keys.clone())
                .unwrap_or_default()
        };
        let mut secret_changes = Vec::new();
        if update.replace
            && let Some(previous) = previous
        {
            for name in &previous.sensitive_user_config_keys {
                if !update.values.contains_key(name) {
                    secret_changes.push(PlannedSecretChange {
                        key: self.storage.secret_key(id, name)?,
                        value: None,
                    });
                }
            }
        }
        for (name, value) in update.values {
            let definition = manifest.user_config.get(&name).ok_or_else(|| {
                ClaudePluginError::Invalid(format!("插件 {} 没有 userConfig 字段 {name}", id))
            })?;
            validate_user_config_value(&name, definition, &value)?;
            if definition.sensitive {
                secret_changes.push(PlannedSecretChange {
                    key: self.storage.secret_key(id, &name)?,
                    value: Some(value),
                });
                public.remove(&name);
                sensitive.insert(name);
            } else {
                public.insert(name.clone(), value);
                if sensitive.remove(&name) {
                    secret_changes.push(PlannedSecretChange {
                        key: self.storage.secret_key(id, &name)?,
                        value: None,
                    });
                }
            }
        }
        if require_complete {
            for (name, definition) in &manifest.user_config {
                if !definition.required {
                    continue;
                }
                let exists = if definition.sensitive {
                    sensitive.contains(name)
                } else {
                    public.contains_key(name)
                };
                if !exists && definition.default.is_none() {
                    return Err(ClaudePluginError::Invalid(format!(
                        "插件 {} 缺少必填 userConfig：{name}",
                        id
                    )));
                }
            }
        }
        Ok(UserConfigPlan {
            public_user_config: public,
            sensitive_user_config_keys: sensitive,
            secret_changes,
        })
    }
}

/// 判断安装记录是否已经具备所有 required userConfig；默认值也算已满足。
fn has_complete_required_user_config(
    manifest: &PluginManifest,
    public: &BTreeMap<String, Value>,
    sensitive: &BTreeSet<String>,
) -> bool {
    manifest.user_config.iter().all(|(name, definition)| {
        !definition.required
            || definition.default.is_some()
            || if definition.sensitive {
                sensitive.contains(name)
            } else {
                public.contains_key(name)
            }
    })
}

/// 供 extensions 和 peri runtime 使用的纯数据快照。
#[derive(Clone, Debug, Default)]
pub struct PluginRuntimeSnapshot {
    /// 按稳定 ID 排序的启用插件投影。
    pub plugins: Vec<RuntimePlugin>,
    /// 已转换为 peri 注册表的插件 Hook。
    pub plugin_hooks: Vec<peri_middlewares::hooks::RegisteredHook>,
}

/// 单个启用插件的运行时投影。
#[derive(Clone, Debug)]
pub struct RuntimePlugin {
    /// 唯一插件 ID。
    pub id: PluginId,
    /// 规范化插件根目录。
    pub root: PathBuf,
    /// 命令 Markdown 文件。
    pub commands: Vec<ComponentFile>,
    /// Skill Markdown 文件。
    pub skills: Vec<ComponentFile>,
    /// Agent Markdown 文件。
    pub agents: Vec<ComponentFile>,
    /// 插件清单 hooks（已插值）。
    pub hooks: Option<Value>,
    /// `hooks` 中声明了但 peri `HookEvent::parse` 无法识别的事件名（如拼写错误或
    /// peri 尚未实现的 Claude Code 事件）；这些事件在运行时会被静默跳过，此字段仅用于向 UI 暴露可见性。
    pub unsupported_hooks: Vec<String>,
    /// `.mcp.json` 与 manifest mcpServers 合并后的配置（已插值）。
    pub mcp_servers: BTreeMap<String, Value>,
    /// manifest lspServers 转换后的 Peri 模板；静态变量已插值，Session 变量延迟绑定。
    pub lsp_servers: Vec<peri_acp_types::lsp::LspServerConfig>,
}

/// 一个可加载的 Claude 命令、Skill 或 Agent 文件。
#[derive(Clone, Debug)]
pub struct ComponentFile {
    /// 文件绝对路径。
    pub path: PathBuf,
    /// 相对插件根目录的可展示路径。
    pub relative_path: PathBuf,
}

/// 将字符串中的 `${NAME}` 替换为变量值；`$$` 不具有特殊含义，未知变量是硬错误。
pub fn interpolate_variables(input: &str, variables: &BTreeMap<String, String>) -> Result<String> {
    let mut output = String::with_capacity(input.len());
    let mut remainder = input;
    while let Some(start) = remainder.find("${") {
        output.push_str(&remainder[..start]);
        let after_start = &remainder[start + 2..];
        let end = after_start
            .find('}')
            .ok_or_else(|| ClaudePluginError::Invalid("变量插值缺少结束大括号".to_owned()))?;
        let name = &after_start[..end];
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            return Err(ClaudePluginError::Invalid(format!("变量名无效：{name}")));
        }
        let value = variables
            .get(name)
            .ok_or_else(|| ClaudePluginError::MissingVariable(name.to_owned()))?;
        output.push_str(value);
        remainder = &after_start[end + 1..];
    }
    output.push_str(remainder);
    Ok(output)
}

/// 递归插值 JSON 中的所有字符串键和值，保持其余 JSON 类型不变。
pub fn interpolate_json(value: &Value, variables: &BTreeMap<String, String>) -> Result<Value> {
    match value {
        Value::String(value) => Ok(Value::String(interpolate_variables(value, variables)?)),
        Value::Array(values) => values
            .iter()
            .map(|value| interpolate_json(value, variables))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(values) => {
            let mut result = Map::new();
            for (key, value) in values {
                result.insert(
                    interpolate_variables(key, variables)?,
                    interpolate_json(value, variables)?,
                );
            }
            Ok(Value::Object(result))
        }
        other => Ok(other.clone()),
    }
}

/// 解析一个市场清单字节串，并执行所有本地结构约束。
pub fn parse_marketplace_manifest(bytes: &[u8]) -> Result<MarketplaceManifest> {
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ClaudePluginError::Invalid(format!(
            "市场清单超过 {} 字节",
            MAX_MANIFEST_BYTES
        )));
    }
    let manifest: MarketplaceManifest = serde_json::from_slice(bytes)?;
    validate_marketplace_manifest(&manifest)?;
    Ok(manifest)
}

/// 解析一个插件清单字节串，并执行所有本地结构约束。
pub fn parse_plugin_manifest(bytes: &[u8]) -> Result<PluginManifest> {
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ClaudePluginError::Invalid(format!(
            "插件清单超过 {} 字节",
            MAX_MANIFEST_BYTES
        )));
    }
    let manifest: PluginManifest = serde_json::from_slice(bytes)?;
    validate_plugin_manifest(&manifest)?;
    Ok(manifest)
}

/// 从市场根目录读取 `.claude-plugin/marketplace.json`。
pub fn load_marketplace_manifest(root: &Path) -> Result<MarketplaceManifest> {
    parse_marketplace_manifest(&read_limited(&root.join(CLAUDE_MARKETPLACE_MANIFEST))?)
}

/// 从插件根目录读取 `.claude-plugin/plugin.json`。
pub fn load_plugin_manifest(root: &Path) -> Result<PluginManifest> {
    let mut manifest = parse_plugin_manifest(&read_limited(&root.join(CLAUDE_PLUGIN_MANIFEST))?)?;
    merge_mcp_bundle_user_config(root, &mut manifest)?;
    // DXT/MCPB schema 是插件运行时 userConfig 的一部分，合并后重新校验
    // required/default/min/max，确保 UI 与 SecretStore 使用同一份定义。
    validate_plugin_manifest(&manifest)?;
    Ok(manifest)
}

/// 将 marketplace 条目的对象形式 lspServers 转换为 Peri 可加载的合成插件清单。
///
/// Peri 3.6.5 用该路径支持官方市场中只有 LSP 声明、没有原生 plugin.json 的插件。
pub fn synthetic_marketplace_plugin_manifest(
    plugin: &MarketplacePlugin,
) -> Result<Option<PluginManifest>> {
    let Some(value) = synthetic_marketplace_plugin_manifest_value(plugin, false)? else {
        return Ok(None);
    };
    Ok(Some(parse_plugin_manifest(&serde_json::to_vec(&value)?)?))
}

/// 为已在本机展开的无清单插件识别 Claude Code 默认组件目录。
///
/// 官方市场的 `receipts`、`session-report` 等条目没有在 marketplace.json
/// 重复声明 `skills`，也没有 plugin.json，但仍按 Claude Code 约定提供
/// `skills/`。只在真实目录至少包含一个可解析组件时生成清单，避免把空目录
/// 误报为可安装插件。
pub fn synthetic_marketplace_plugin_manifest_for_root(
    plugin: &MarketplacePlugin,
    source_root: &Path,
) -> Result<Option<PluginManifest>> {
    let use_default_components = has_default_component_layout(source_root)?;
    let Some(value) = synthetic_marketplace_plugin_manifest_value(plugin, use_default_components)?
    else {
        return Ok(None);
    };
    Ok(Some(parse_plugin_manifest(&serde_json::to_vec(&value)?)?))
}

/// 复制无原生清单的 marketplace 插件，并在受控缓存副本中写入合成 plugin.json。
pub fn materialize_synthetic_marketplace_plugin(
    source_root: &Path,
    destination: &Path,
    plugin: &MarketplacePlugin,
) -> Result<PathBuf> {
    let source_root = fs::canonicalize(source_root)?;
    if !source_root.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "市场插件来源不是目录：{}",
            source_root.display()
        )));
    }
    let use_default_components = has_default_component_layout(&source_root)?;
    let manifest = synthetic_marketplace_plugin_manifest_value(plugin, use_default_components)?
        .ok_or_else(|| {
            ClaudePluginError::Invalid(format!(
                "市场插件 {} 缺少 .claude-plugin/plugin.json、可识别的默认组件目录、skills 与 lspServers",
                plugin.name
            ))
        })?;
    copy_plugin_tree(&source_root, destination)?;
    let manifest_dir = destination.join(".claude-plugin");
    fs::create_dir_all(&manifest_dir)?;
    fs::write(
        manifest_dir.join("plugin.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    // 写入后再走唯一解析器，确保磁盘内容与内存校验没有分叉。
    load_plugin_manifest(destination)?;
    Ok(fs::canonicalize(destination)?)
}

/// 构造 marketplace 合成清单的原始 JSON。
///
/// Claude Code 官方市场允许 `strict:false` 条目直接声明 `skills`，这类 skill
/// bundle 通常没有 `.claude-plugin/plugin.json`。这里把它们转换为当前运行时能
/// 读取的标准清单；同时保留没有原生清单的 LSP-only 条目行为。
fn synthetic_marketplace_plugin_manifest_value(
    plugin: &MarketplacePlugin,
    use_default_components: bool,
) -> Result<Option<Value>> {
    let skills = plugin
        .extra
        .get("skills")
        .map(|value| -> Result<Option<Vec<String>>> {
            let declaration = serde_json::from_value::<ComponentDeclaration>(value.clone())
                .map_err(|error| {
                    ClaudePluginError::Invalid(format!(
                        "市场插件 {} 的 skills 必须是有效的组件路径声明：{error}",
                        plugin.name
                    ))
                })?;
            if declaration.paths.is_empty() {
                Ok(None)
            } else {
                Ok(Some(declaration.paths))
            }
        })
        .transpose()?
        .flatten();
    let lsp_value = plugin.extra.get("lspServers");
    if skills.is_none() && lsp_value.is_none() && !use_default_components {
        return Ok(None);
    }
    let lsp_servers = if let Some(lsp_value) = lsp_value {
        let lsp_map = lsp_value.as_object().ok_or_else(|| {
            ClaudePluginError::Invalid(format!("市场插件 {} 的 lspServers 必须是对象", plugin.name))
        })?;
        if lsp_map.is_empty() {
            return Err(ClaudePluginError::Invalid(format!(
                "市场插件 {} 的 lspServers 不能为空",
                plugin.name
            )));
        }
        let mut lsp_servers = Vec::with_capacity(lsp_map.len());
        for (server_name, server_value) in lsp_map {
            let mut server = server_value.as_object().cloned().ok_or_else(|| {
                ClaudePluginError::Invalid(format!(
                    "市场插件 {} 的 LSP Server {server_name} 必须是对象",
                    plugin.name
                ))
            })?;
            if let Some(declared_name) = server.get("name") {
                let declared_name = declared_name.as_str().ok_or_else(|| {
                    ClaudePluginError::Invalid(format!(
                        "市场插件 {} 的 LSP Server {server_name} name 必须是字符串",
                        plugin.name
                    ))
                })?;
                if declared_name != server_name {
                    return Err(ClaudePluginError::Invalid(format!(
                        "市场插件 {} 的 LSP Server 名称与对象键不一致：{server_name}",
                        plugin.name
                    )));
                }
            }
            server.insert("name".to_owned(), Value::String(server_name.clone()));
            lsp_servers.push(Value::Object(server));
        }
        Some(lsp_servers)
    } else {
        None
    };

    let mut manifest = Map::new();
    manifest.insert("name".to_owned(), Value::String(plugin.name.clone()));
    if let Some(version) = &plugin.version {
        manifest.insert("version".to_owned(), Value::String(version.clone()));
    }
    if let Some(description) = &plugin.description {
        manifest.insert("description".to_owned(), Value::String(description.clone()));
    }
    if let Some(mcp_servers) = plugin.extra.get("mcpServers") {
        manifest.insert("mcpServers".to_owned(), mcp_servers.clone());
    }
    if let Some(skills) = skills {
        manifest.insert(
            "skills".to_owned(),
            Value::Array(skills.into_iter().map(Value::String).collect()),
        );
    }
    if let Some(lsp_servers) = lsp_servers {
        manifest.insert("lspServers".to_owned(), Value::Array(lsp_servers));
    }
    let value = Value::Object(manifest);
    // 合成阶段立即使用同一严格解析器验证，禁止把坏清单写入缓存。
    parse_plugin_manifest(&serde_json::to_vec(&value)?)?;
    Ok(Some(value))
}

/// Claude Code 在 plugin.json 省略组件字段时会扫描这三个约定目录。
/// 复用运行时的严格扫描器确认至少存在一个真实组件，并同时拒绝越界路径和
/// 符号链接；只有通过检查的目录才允许生成 name-only 清单。
fn has_default_component_layout(source_root: &Path) -> Result<bool> {
    let root = fs::canonicalize(source_root)?;
    if !root.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "市场插件来源不是目录：{}",
            root.display()
        )));
    }
    for directory in ["commands", "skills", "agents"] {
        if !scan_declared_or_default_components(&root, &[], directory)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 已完成命名空间、重复名称与请求条目校验的市场索引。
pub(crate) struct ValidatedMarketplaceIndex<'a> {
    /// 经过严格标识校验的市场名称。
    pub(crate) marketplace_name: String,
    /// 按忽略大小写键索引的唯一市场条目。
    pub(crate) plugins: HashMap<String, &'a MarketplacePlugin>,
    /// 恢复清单规范大小写后的请求插件标识。
    pub(crate) requested: PluginId,
}

/// 统一校验请求命名空间并构建忽略大小写的市场插件索引。
pub(crate) fn validated_marketplace_index<'a>(
    requested: &PluginId,
    marketplace: &'a MarketplaceManifest,
) -> Result<ValidatedMarketplaceIndex<'a>> {
    let requested = PluginId::from_components(&requested.plugin, requested.marketplace.as_deref())?;
    let marketplace_name = normalized_identifier(&marketplace.name, "市场名称")?;
    if let Some(namespace) = requested.marketplace.as_deref()
        && !namespace.eq_ignore_ascii_case(&marketplace_name)
    {
        return Err(ClaudePluginError::Invalid(format!(
            "请求插件市场 {namespace} 与当前市场 {marketplace_name} 不一致"
        )));
    }
    let plugins = marketplace
        .plugins
        .iter()
        .try_fold(HashMap::new(), |mut entries, entry| {
            let key = marketplace_name_key(&entry.name);
            if entries.insert(key, entry).is_some() {
                return Err(ClaudePluginError::Invalid(format!(
                    "市场插件名称重复（忽略大小写）：{}",
                    entry.name
                )));
            }
            Ok(entries)
        })?;
    let requested_entry = plugins
        .get(&marketplace_name_key(&requested.plugin))
        .ok_or_else(|| {
            ClaudePluginError::Invalid(format!(
                "市场 {marketplace_name} 中不存在插件 {}",
                requested.plugin
            ))
        })?;
    Ok(ValidatedMarketplaceIndex {
        requested: PluginId::from_components(&requested_entry.name, Some(&marketplace_name))?,
        marketplace_name,
        plugins,
    })
}

/// 根据市场内所有插件的清单与市场字段构建依赖闭包，返回依赖在前的拓扑顺序。
pub fn dependency_closure(
    requested: &PluginId,
    marketplace: &MarketplaceManifest,
    manifests: &BTreeMap<String, PluginManifest>,
) -> Result<Vec<PluginId>> {
    let ValidatedMarketplaceIndex {
        marketplace_name,
        plugins: market_plugins,
        requested,
    } = validated_marketplace_index(requested, marketplace)?;
    let manifests = manifests
        .iter()
        .try_fold(HashMap::new(), |mut parsed, (name, manifest)| {
            let key = marketplace_name_key(name);
            if parsed.insert(key, manifest).is_some() {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件清单名称重复（忽略大小写）：{}",
                    name
                )));
            }
            Ok(parsed)
        })?;
    let mut result = Vec::new();
    let mut visiting = Vec::new();
    let mut complete = BTreeSet::new();
    visit_dependency(
        &requested,
        &marketplace_name,
        &market_plugins,
        &manifests,
        &mut visiting,
        &mut complete,
        &mut result,
    )?;
    Ok(result)
}

/// 扫描插件目录并抽取 Claude commands/skills/agents/hooks/MCP 组件。
pub fn extract_components(
    id: PluginId,
    root: &Path,
    manifest: &PluginManifest,
    project_dir: &Path,
    environment: &BTreeMap<String, String>,
    config: &ResolvedUserConfig,
) -> Result<RuntimePlugin> {
    let root = canonical_plugin_root(root)?;
    let mut variables = environment.clone();
    variables.insert("CLAUDE_PLUGIN_ROOT".to_owned(), path_to_frontend(&root));
    variables.insert(
        "CLAUDE_PLUGIN_DATA".to_owned(),
        path_to_frontend(&root.join("data")),
    );
    variables.insert(
        "CLAUDE_SKILL_DIR".to_owned(),
        path_to_frontend(&root.join("skills")),
    );
    variables
        .entry("CLAUDE_SESSION_ID".to_owned())
        .or_insert_with(|| {
            environment
                .get("CLAUDE_SESSION_ID")
                .cloned()
                .unwrap_or_default()
        });
    variables.insert(
        "CLAUDE_PROJECT_DIR".to_owned(),
        path_to_frontend(project_dir),
    );
    variables.insert("CLAUDE_PLUGIN_ID".to_owned(), id.to_string());
    variables.insert("CLAUDE_PLUGIN_NAME".to_owned(), id.plugin.clone());
    if let Some(marketplace) = &id.marketplace {
        variables.insert("CLAUDE_PLUGIN_MARKETPLACE".to_owned(), marketplace.clone());
    }
    if let Some(version) = &manifest.version {
        variables.insert("CLAUDE_PLUGIN_VERSION".to_owned(), version.clone());
    }
    for (name, value) in &config.values {
        if let Some(value) = config_value_as_variable(value) {
            variables.insert(
                format!("CLAUDE_PLUGIN_{}", normalize_variable_name(name)),
                value.clone(),
            );
            variables.insert(format!("user_config.{name}"), value);
        }
    }
    let commands =
        scan_declared_or_default_components(&root, &manifest.commands.paths, "commands")?;
    let skills = scan_declared_or_default_components(&root, &manifest.skills.paths, "skills")?;
    let agents = scan_declared_or_default_components(&root, &manifest.agents.paths, "agents")?;
    let hooks = load_hooks(&root, manifest.hooks.as_ref(), &variables)?;
    let unsupported_hooks = unsupported_hook_events(hooks.as_ref());
    let mut mcp_servers = BTreeMap::new();
    if let Some(file_servers) = load_mcp_file(&root)? {
        mcp_servers.extend(file_servers);
    }
    for file in &manifest.mcp_servers.files {
        mcp_servers.extend(load_mcp_servers_file(&root, file)?);
    }
    mcp_servers.extend(manifest.mcp_servers.inline.clone());
    let mcp_servers = mcp_servers
        .into_iter()
        .map(|(name, value)| {
            let value = interpolate_json(&value, &variables)?;
            Ok((name, normalize_mcp_server_value(value)?))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut lsp_variables = variables.clone();
    // cwd 与 Session ID 只能在 Peri 创建具体 Session 时确定；加载期只展开
    // 插件根、用户配置等静态变量，避免所有 Session 被启动目录或旧环境值污染。
    lsp_variables.insert(
        "CLAUDE_PROJECT_DIR".to_owned(),
        "${CLAUDE_PROJECT_DIR}".to_owned(),
    );
    lsp_variables.insert(
        "CLAUDE_SESSION_ID".to_owned(),
        "${CLAUDE_SESSION_ID}".to_owned(),
    );
    let lsp_servers = manifest
        .lsp_servers
        .iter()
        .map(|server| {
            let command = interpolate_variables(&server.command, &lsp_variables)?;
            let args = server
                .args
                .iter()
                .map(|argument| interpolate_variables(argument, &lsp_variables))
                .collect::<Result<Vec<_>>>()?;
            let environment = server
                .env
                .iter()
                .map(|(name, value)| {
                    Ok((name.clone(), interpolate_variables(value, &lsp_variables)?))
                })
                .collect::<Result<Vec<_>>>()?;
            let initialization_options = server
                .initialization_options
                .as_ref()
                .map(|value| interpolate_json(value, &lsp_variables))
                .transpose()?;
            let mut config = peri_resources::lsp::config::lsp_config_from_plugin(
                &id.plugin,
                &server.name,
                &command,
                &args,
                &root,
                server.extension_to_language.clone().into_iter().collect(),
            );
            let config_environment = config.env.get_or_insert_default();
            config_environment.extend(environment);
            // 插件不能用清单字段伪造安装根；该保留变量始终由宿主注入真实路径。
            config_environment.insert("CLAUDE_PLUGIN_ROOT".to_owned(), path_to_frontend(&root));
            config.initialization_options = initialization_options;
            config.disabled = server.disabled;
            config.max_restarts = server.max_restarts;
            config.startup_timeout = server.startup_timeout;
            Ok(config)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(RuntimePlugin {
        id,
        root,
        commands,
        skills,
        agents,
        hooks,
        unsupported_hooks,
        mcp_servers,
        lsp_servers,
    })
}

/// 提取 `hooks` 声明中 peri `HookEvent::parse` 无法识别的事件名，按原始顺序去重。
fn unsupported_hook_events(hooks: Option<&Value>) -> Vec<String> {
    let Some(Value::Object(events)) = hooks else {
        return Vec::new();
    };
    events
        .keys()
        .filter(|event_name| peri_middlewares::hooks::HookEvent::parse(event_name).is_none())
        .cloned()
        .collect()
}

/// 已解析的用户配置；只在进程内向变量插值提供敏感值，调用方不得序列化或记录它。
#[derive(Clone, Debug, Default)]
pub struct ResolvedUserConfig {
    /// 类型校验后的配置值，可能包含来自安全存储的敏感值。
    pub values: BTreeMap<String, Value>,
    /// 密钥库没有返回的敏感必填字段。
    pub missing_sensitive: BTreeSet<String>,
}

/// 读取公开配置与安全存储，填充默认值并再次校验值类型。
pub fn resolved_user_config(
    storage: &PluginStorage,
    installed: &InstalledPlugin,
    manifest: &PluginManifest,
    secrets: &dyn SecretStore,
) -> Result<ResolvedUserConfig> {
    let mut result = ResolvedUserConfig::default();
    for (name, definition) in &manifest.user_config {
        let value = if definition.sensitive {
            secrets.get_json(&storage.secret_key_at(
                &installed.id,
                name,
                installed.secret_generation,
            )?)?
        } else {
            installed.public_user_config.get(name).cloned()
        }
        .or_else(|| definition.default.clone());
        match value {
            Some(value) => {
                validate_user_config_value(name, definition, &value)?;
                result.values.insert(name.clone(), value);
            }
            None if definition.sensitive && definition.required => {
                result.missing_sensitive.insert(name.clone());
            }
            None if definition.required => {
                return Err(ClaudePluginError::Invalid(format!(
                    "插件 {} 缺少必填 userConfig：{name}",
                    installed.id
                )));
            }
            None => {}
        }
    }
    Ok(result)
}

/// 验证市场清单的唯一命名空间、条目名称和来源。
fn validate_marketplace_manifest(manifest: &MarketplaceManifest) -> Result<()> {
    normalized_identifier(&manifest.name, "市场名称")?;
    validate_marketplace_name(&manifest.name)?;
    let mut names = BTreeSet::new();
    for plugin in &manifest.plugins {
        let name = normalized_identifier(&plugin.name, "市场插件名称")?;
        if !names.insert(marketplace_name_key(&name)) {
            return Err(ClaudePluginError::Invalid(format!(
                "市场插件名称重复（忽略大小写）：{name}"
            )));
        }
        validate_plugin_source(&plugin.source)?;
        validate_dependency_names(&plugin.dependencies)?;
    }
    Ok(())
}

/// 市场名称与官方命名空间的关系。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MarketplaceNameClass {
    /// 普通第三方市场名称。
    Ordinary,
    /// Anthropic 官方保留市场名称。
    Official,
}

/// 分类市场名称并拒绝保留命名空间或官方冒充名称。
fn classify_marketplace_name(name: &str) -> Result<MarketplaceNameClass> {
    let normalized = normalized_identifier(name, "市场名称")?;
    let lower = normalized.to_ascii_lowercase();
    if matches!(lower.as_str(), "builtin" | "inline") {
        return Err(ClaudePluginError::Invalid(format!(
            "市场名称 {name} 是 Claude 保留命名空间"
        )));
    }
    let official = matches!(
        lower.as_str(),
        "claude-code-marketplace"
            | "claude-code-plugins"
            | "claude-plugins-official"
            | "anthropic-marketplace"
            | "anthropic-plugins"
            | "agent-skills"
            | "life-sciences"
            | "knowledge-work-plugins"
    );
    let impersonation = lower.contains("official")
        && (lower.contains("claude") || lower.contains("anthropic"))
        || ((lower.starts_with("claude") || lower.starts_with("anthropic"))
            && (lower.contains("marketplace") || lower.contains("plugins")));
    if impersonation && !official {
        return Err(ClaudePluginError::Invalid(format!(
            "市场名称 {name} 可能冒充 Claude/Anthropic 官方市场"
        )));
    }
    Ok(if official {
        MarketplaceNameClass::Official
    } else {
        MarketplaceNameClass::Ordinary
    })
}

/// 校验 Claude 保留市场名称，阻止第三方伪装成官方 Anthropic 市场。
pub fn validate_marketplace_name(name: &str) -> Result<()> {
    classify_marketplace_name(name).map(|_| ())
}

/// 校验保留官方名称只能与 Anthropic 官方 GitHub 来源绑定。
pub fn validate_marketplace_name_source(name: &str, source: &str) -> Result<()> {
    if classify_marketplace_name(name)? == MarketplaceNameClass::Official
        && !is_anthropic_github_source(source)
    {
        return Err(ClaudePluginError::Invalid(format!(
            "官方保留市场 {name} 只能来自 github.com/anthropics"
        )));
    }
    Ok(())
}

/// 严格解析官方市场来源，禁止仅凭字符串包含关系绕过 Anthropic owner 校验。
fn is_anthropic_github_source(source: &str) -> bool {
    let source = source.trim();
    if let Some(repository) = source.strip_prefix("github:") {
        return is_anthropic_repository(repository);
    }
    if let Some(repository) = source.strip_prefix("git@github.com:") {
        return is_anthropic_repository(repository);
    }
    let source = source.strip_prefix("git:").unwrap_or(source);
    let Ok(url) = url::Url::parse(source) else {
        return false;
    };
    if url.scheme() != "https"
        || !url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("github.com"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(mut segments) = url.path_segments() else {
        return false;
    };
    let (Some(owner), Some(repository), None) = (segments.next(), segments.next(), segments.next())
    else {
        return false;
    };
    owner.eq_ignore_ascii_case("anthropics") && is_repository_component(repository)
}

/// 校验 `github:owner/repo[@ref]` 与 SSH `owner/repo` 的 owner/repo 结构。
fn is_anthropic_repository(value: &str) -> bool {
    let value = match value.rsplit_once('@') {
        Some((repository, reference)) if !reference.is_empty() => repository,
        Some(_) => return false,
        None => value,
    };
    let mut segments = value.split('/');
    let (Some(owner), Some(repository), None) = (segments.next(), segments.next(), segments.next())
    else {
        return false;
    };
    owner.eq_ignore_ascii_case("anthropics") && is_repository_component(repository)
}

/// GitHub repository 名称不能借助路径、控制字符或空白伪造 owner/repo。
fn is_repository_component(value: &str) -> bool {
    let value = value.strip_suffix(".git").unwrap_or(value);
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// 验证插件清单的版本、路径、配置和依赖。
fn validate_plugin_manifest(manifest: &PluginManifest) -> Result<()> {
    normalized_identifier(&manifest.name, "插件名称")?;
    if let Some(version) = &manifest.version {
        non_empty(version, "插件版本")?;
    }
    validate_component_paths(&manifest.commands.paths, "commands")?;
    validate_component_paths(&manifest.skills.paths, "skills")?;
    validate_component_paths(&manifest.agents.paths, "agents")?;
    validate_dependency_names(&manifest.dependencies)?;
    let mut lsp_names = BTreeSet::new();
    for server in &manifest.lsp_servers {
        let name = normalized_identifier(&server.name, "LSP Server 名称")?;
        if name != server.name {
            return Err(ClaudePluginError::Invalid(format!(
                "LSP Server 名称不能包含首尾空白：{}",
                server.name
            )));
        }
        if !lsp_names.insert(name.clone()) {
            return Err(ClaudePluginError::Invalid(format!(
                "lspServers 包含重复名称：{name}"
            )));
        }
        if server.command.trim().is_empty() {
            return Err(ClaudePluginError::Invalid(format!(
                "LSP Server {name} 的 command 不能为空"
            )));
        }
        if server
            .command
            .chars()
            .any(|character| matches!(character, '\0' | '\r' | '\n'))
        {
            return Err(ClaudePluginError::Invalid(format!(
                "LSP Server {name} 的 command 包含控制字符"
            )));
        }
        if server.args.iter().any(|argument| argument.contains('\0')) {
            return Err(ClaudePluginError::Invalid(format!(
                "LSP Server {name} 的 args 包含空字符"
            )));
        }
        for (extension, language) in &server.extension_to_language {
            if extension.trim().is_empty() || language.trim().is_empty() {
                return Err(ClaudePluginError::Invalid(format!(
                    "LSP Server {name} 的 extensionToLanguage 不能为空"
                )));
            }
        }
    }
    for (name, definition) in &manifest.user_config {
        normalized_identifier(name, "userConfig 字段")?;
        if definition.min.is_some_and(|value| !value.is_finite())
            || definition.max.is_some_and(|value| !value.is_finite())
            || matches!((definition.min, definition.max), (Some(min), Some(max)) if min > max)
        {
            return Err(ClaudePluginError::Invalid(format!(
                "userConfig {name} 的 min/max 无效"
            )));
        }
        if definition.value_type == UserConfigType::Select && definition.enum_values.is_empty() {
            return Err(ClaudePluginError::Invalid(format!(
                "select userConfig 必须声明 enum：{name}"
            )));
        }
        if let Some(default) = &definition.default {
            validate_user_config_value(name, definition, default)?;
        }
    }
    Ok(())
}

/// 仅校验插件来源自身，不在解析清单时访问 settings 或网络。
fn validate_plugin_source(source: &PluginSource) -> Result<()> {
    match source {
        PluginSource::Relative { path } => {
            safe_relative_path(path, "插件相对路径")?;
        }
        PluginSource::Npm {
            package,
            version,
            registry,
        } => {
            package_spec(package, version.as_deref())?;
            validate_optional_registry(registry.as_deref())?;
        }
        PluginSource::Pip {
            package,
            version,
            registry,
        } => {
            pip_package_spec(package, version.as_deref())?;
            validate_optional_registry(registry.as_deref())?;
        }
        PluginSource::Url {
            url,
            reference,
            sha,
        } => {
            non_empty(url, "插件 Git URL")?;
            if let Some(reference) = reference {
                non_empty(reference, "插件 Git ref")?;
            }
            validate_git_sha(sha.as_deref())?;
        }
        PluginSource::Github { repo, sha, .. } => {
            github_git_url(repo)?;
            validate_git_sha(sha.as_deref())?;
        }
        PluginSource::GitSubdir { url, path, sha, .. } => {
            non_empty(url, "Git URL")?;
            safe_relative_path(path, "Git 子目录")?;
            validate_git_sha(sha.as_deref())?;
        }
    }
    Ok(())
}

/// 校验依赖键都可以解析为插件 ID。
fn validate_dependency_names(dependencies: &BTreeMap<String, VersionRequirement>) -> Result<()> {
    for (id, requirement) in dependencies {
        PluginId::parse(id)?;
        if requirement.0.trim().is_empty() {
            return Err(ClaudePluginError::Invalid(format!(
                "依赖版本要求不能为空：{id}"
            )));
        }
    }
    Ok(())
}

/// 递归构建依赖顺序并检测当前位置栈中的循环。
fn visit_dependency(
    id: &PluginId,
    marketplace: &str,
    market_plugins: &HashMap<String, &MarketplacePlugin>,
    manifests: &HashMap<String, &PluginManifest>,
    visiting: &mut Vec<PluginId>,
    complete: &mut BTreeSet<PluginId>,
    result: &mut Vec<PluginId>,
) -> Result<()> {
    if complete.contains(id) {
        return Ok(());
    }
    if let Some(start) = visiting.iter().position(|current| current == id) {
        let mut cycle = visiting[start..].to_vec();
        cycle.push(id.clone());
        return Err(ClaudePluginError::DependencyCycle(cycle));
    }
    let entry = market_plugins
        .get(&marketplace_name_key(&id.plugin))
        .ok_or_else(|| {
            ClaudePluginError::Invalid(format!("市场 {marketplace} 中找不到依赖 {id}"))
        })?;
    let manifest = manifests
        .get(&marketplace_name_key(&id.plugin))
        .ok_or_else(|| {
            ClaudePluginError::Invalid(format!("没有已解析的插件清单，无法计算依赖：{id}"))
        })?;
    visiting.push(id.clone());
    let mut dependencies = entry.dependencies.clone();
    dependencies.extend(manifest.dependencies.clone());
    for dependency in dependencies.keys() {
        let parsed = PluginId::parse(dependency)?;
        let dependency = match parsed.marketplace.as_deref() {
            None => canonical_dependency_id(&parsed, marketplace, market_plugins)?,
            Some(namespace) if namespace.eq_ignore_ascii_case(marketplace) => {
                canonical_dependency_id(&parsed, marketplace, market_plugins)?
            }
            Some(namespace) => {
                return Err(ClaudePluginError::Invalid(format!(
                    "跨市场依赖 {dependency}@{namespace} 需要由上层市场解析器提供"
                )));
            }
        };
        visit_dependency(
            &dependency,
            marketplace,
            market_plugins,
            manifests,
            visiting,
            complete,
            result,
        )?;
    }
    visiting.pop();
    complete.insert(id.clone());
    result.push(id.clone());
    Ok(())
}

/// 将依赖引用按市场条目的 canonical 名称投影，名称比较使用 ASCII 折叠。
fn canonical_dependency_id(
    parsed: &PluginId,
    marketplace: &str,
    market_plugins: &HashMap<String, &MarketplacePlugin>,
) -> Result<PluginId> {
    let entry = market_plugins
        .get(&marketplace_name_key(&parsed.plugin))
        .ok_or_else(|| {
            ClaudePluginError::Invalid(format!(
                "市场 {marketplace} 中找不到依赖 {}@{}",
                parsed.plugin, marketplace
            ))
        })?;
    PluginId::from_components(&entry.name, Some(marketplace)).map_err(Into::into)
}

/// 读取并限制清单文件大小。
fn read_limited(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(ClaudePluginError::Invalid(format!(
            "不允许读取符号链接清单：{}",
            path.display()
        )));
    }
    if !metadata.is_file() {
        return Err(ClaudePluginError::Invalid(format!(
            "不是普通文件：{}",
            path.display()
        )));
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(ClaudePluginError::Invalid(format!(
            "文件超过 {} 字节：{}",
            MAX_MANIFEST_BYTES,
            path.display()
        )));
    }
    Ok(fs::read(path)?)
}

/// 解析市场 source 的完整 JSON 表示。
fn parse_marketplace_source(value: Value) -> Result<MarketplaceSource> {
    if let Value::String(value) = value {
        return Ok(MarketplaceSource::Directory { path: value });
    }
    let object = value.as_object().ok_or_else(|| {
        ClaudePluginError::Invalid("市场 source 必须是路径字符串或对象".to_owned())
    })?;
    let source = object_string(object, "source")?;
    match source.as_str() {
        "url" => Ok(MarketplaceSource::Url {
            url: object_string(object, "url")?,
            headers: optional_string_map(object, "headers")?,
        }),
        "github" => Ok(MarketplaceSource::Github {
            repo: object_string(object, "repo")?,
            reference: optional_string(object, "ref")?,
            path: optional_string(object, "path")?,
            sparse_paths: optional_string_array(object, "sparsePaths")?,
        }),
        "git" => Ok(MarketplaceSource::Git {
            url: object_string(object, "url")?,
            reference: optional_string(object, "ref")?,
            path: optional_string(object, "path")?,
            sparse_paths: optional_string_array(object, "sparsePaths")?,
        }),
        "npm" => Ok(MarketplaceSource::Npm {
            package: object_string(object, "package")?,
            version: optional_string(object, "version")?,
            registry: optional_string(object, "registry")?,
        }),
        "file" => Ok(MarketplaceSource::File {
            path: object_string(object, "path")?,
        }),
        "directory" => Ok(MarketplaceSource::Directory {
            path: object_string(object, "path")?,
        }),
        "settings" => Ok(MarketplaceSource::Settings {
            key: object
                .get("key")
                .or_else(|| object.get("path"))
                .and_then(Value::as_str)
                .ok_or_else(|| ClaudePluginError::Invalid("settings 市场来源缺少 key".to_owned()))?
                .to_owned(),
        }),
        other => Err(ClaudePluginError::Invalid(format!(
            "不支持的市场 source：{other}"
        ))),
    }
}

/// 解析插件 source 的完整 JSON 表示。
fn parse_plugin_source(value: Value) -> Result<PluginSource> {
    if let Value::String(value) = value {
        return Ok(PluginSource::Relative { path: value });
    }
    let object = value.as_object().ok_or_else(|| {
        ClaudePluginError::Invalid("插件 source 必须是相对路径字符串或对象".to_owned())
    })?;
    let source = object_string(object, "source")?;
    match source.as_str() {
        "npm" => Ok(PluginSource::Npm {
            package: object_string(object, "package")?,
            version: optional_string(object, "version")?,
            registry: optional_string(object, "registry")?,
        }),
        "url" => Ok(PluginSource::Url {
            url: object_string(object, "url")?,
            reference: optional_string(object, "ref")?,
            sha: optional_string(object, "sha")?,
        }),
        "github" => Ok(PluginSource::Github {
            repo: object_string(object, "repo")?,
            reference: optional_string(object, "ref")?,
            sha: optional_string(object, "sha")?,
        }),
        "git-subdir" => Ok(PluginSource::GitSubdir {
            url: object_string(object, "url")?,
            path: object_string(object, "path")?,
            reference: optional_string(object, "ref")?,
            sha: optional_string(object, "sha")?,
        }),
        "pip" => Ok(PluginSource::Pip {
            package: object_string(object, "package")?,
            version: optional_string(object, "version")?,
            registry: optional_string(object, "registry")?,
        }),
        other => Err(ClaudePluginError::Invalid(format!(
            "不支持的插件 source：{other}"
        ))),
    }
}

/// 解析清单 `mcpServers` 的内联对象、数组和相对 JSON 文件形式。
fn parse_mcp_servers_declaration(value: Value) -> Result<McpServersDeclaration> {
    match value {
        Value::Null => Ok(McpServersDeclaration::default()),
        Value::String(file) => {
            validate_mcp_reference(&file)?;
            Ok(McpServersDeclaration {
                inline: BTreeMap::new(),
                files: vec![file],
            })
        }
        Value::Array(values) => {
            let mut declaration = McpServersDeclaration::default();
            let mut inline = Vec::new();
            for value in values {
                match value {
                    Value::String(file) => {
                        validate_mcp_reference(&file)?;
                        declaration.files.push(file);
                    }
                    value => inline.push(value),
                }
            }
            if !inline.is_empty() {
                declaration.inline = parse_mcp_servers_entries(Value::Array(inline))?;
            }
            Ok(declaration)
        }
        Value::Object(mut object) => {
            let mut declaration = McpServersDeclaration::default();
            if let Some(files) = object.remove("file").or_else(|| object.remove("files")) {
                let files = match files {
                    Value::String(file) => vec![file],
                    Value::Array(values) => values
                        .into_iter()
                        .map(|value| {
                            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                                ClaudePluginError::Invalid(
                                    "mcpServers files 必须是字符串数组".to_owned(),
                                )
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                    _ => {
                        return Err(ClaudePluginError::Invalid(
                            "mcpServers file 必须是字符串或字符串数组".to_owned(),
                        ));
                    }
                };
                for file in files {
                    validate_mcp_reference(&file)?;
                    declaration.files.push(file);
                }
            }
            if let Some(servers) = object
                .remove("servers")
                .or_else(|| object.remove("mcpServers"))
            {
                declaration.inline = parse_mcp_servers_entries(servers)?;
            }
            if !object.is_empty() {
                for (name, value) in parse_mcp_servers_entries(Value::Object(object))? {
                    if declaration.inline.insert(name.clone(), value).is_some() {
                        return Err(ClaudePluginError::Invalid(format!(
                            "mcpServers 混合声明包含重复 Server：{name}"
                        )));
                    }
                }
            }
            Ok(declaration)
        }
        _ => Err(ClaudePluginError::Invalid(
            "mcpServers 必须是对象、数组或相对文件路径".to_owned(),
        )),
    }
}

/// 校验 MCP JSON 或 MCPB/DXT 引用，并拒绝任意路径与不安全 URL。
fn validate_mcp_reference(file: &str) -> Result<()> {
    if file.starts_with("http://") || file.starts_with("https://") {
        let parsed = url::Url::parse(file)
            .map_err(|_| ClaudePluginError::Invalid("MCPB/DXT URL 格式无效".to_owned()))?;
        let extension = parsed
            .path()
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matches!(parsed.scheme(), "http" | "https")
            || !matches!(extension.as_str(), "mcpb" | "dxt")
        {
            return Err(ClaudePluginError::Invalid(
                "远程 MCPB/DXT 必须是 http(s) URL 且以 .mcpb 或 .dxt 结尾".to_owned(),
            ));
        }
        return Ok(());
    }
    safe_relative_path(file, "mcpServers 文件")?;
    Ok(())
}

/// 从 JSON 对象取必填字符串字段。
fn object_string(object: &Map<String, Value>, key: &str) -> Result<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ClaudePluginError::Invalid(format!("source 缺少非空 string {key}")))
}

/// 从 JSON 对象取可选字符串字段。
fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(ClaudePluginError::Invalid(format!(
            "source {key} 必须是非空字符串"
        ))),
    }
}

/// 从 JSON 对象取可选的字符串数组字段，并拒绝空项、非字符串和路径穿越项。
fn optional_string_array(object: &Map<String, Value>, key: &str) -> Result<Vec<String>> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    let Value::Array(values) = value else {
        return Err(ClaudePluginError::Invalid(format!(
            "source {key} 必须是字符串数组"
        )));
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    ClaudePluginError::Invalid(format!("source {key} 必须是非空字符串数组"))
                })
        })
        .collect()
}

/// 从 JSON 对象取可选的 HTTP 头映射，避免把非字符串值传给网络层。
fn optional_string_map(object: &Map<String, Value>, key: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = object.get(key) else {
        return Ok(BTreeMap::new());
    };
    let Value::Object(values) = value else {
        return Err(ClaudePluginError::Invalid(format!(
            "source {key} 必须是字符串对象"
        )));
    };
    let mut output = BTreeMap::new();
    for (name, value) in values {
        let value = value.as_str().ok_or_else(|| {
            ClaudePluginError::Invalid(format!("source {key}.{name} 必须是字符串"))
        })?;
        if name.trim().is_empty() || value.trim().is_empty() {
            return Err(ClaudePluginError::Invalid(format!(
                "source {key} 的名称和值不能为空"
            )));
        }
        output.insert(name.clone(), value.to_owned());
    }
    Ok(output)
}

/// 将 dependencies 的对象、字符串数组或对象数组归一为 map。
fn deserialize_dependencies<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, VersionRequirement>, D::Error>
where
    D: Deserializer<'de>,
{
    let value =
        Option::<Value>::deserialize(deserializer)?.unwrap_or_else(|| Value::Object(Map::new()));
    let mut output = BTreeMap::new();
    match value {
        Value::Object(values) => {
            for (name, version) in values {
                let version = match version {
                    Value::String(version) => version,
                    Value::Object(object) => object
                        .get("version")
                        .and_then(Value::as_str)
                        .unwrap_or("*")
                        .to_owned(),
                    Value::Null => "*".to_owned(),
                    _ => {
                        return Err(serde::de::Error::custom(
                            "dependencies 值必须为字符串或对象",
                        ));
                    }
                };
                output.insert(name, VersionRequirement(version));
            }
        }
        Value::Array(values) => {
            for value in values {
                match value {
                    Value::String(name) => {
                        output.insert(name, VersionRequirement("*".to_owned()));
                    }
                    Value::Object(object) => {
                        let name = object
                            .get("name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| serde::de::Error::custom("依赖对象缺少 name"))?;
                        let version = object.get("version").and_then(Value::as_str).unwrap_or("*");
                        output.insert(name.to_owned(), VersionRequirement(version.to_owned()));
                    }
                    _ => {
                        return Err(serde::de::Error::custom(
                            "dependencies 数组项必须为字符串或对象",
                        ));
                    }
                }
            }
        }
        Value::Null => {}
        _ => return Err(serde::de::Error::custom("dependencies 必须为对象或数组")),
    }
    Ok(output)
}

/// 标准化且校验一个公开标识符。
fn normalized_identifier(value: &str, label: &str) -> Result<String> {
    peri_acp_types::plugin::normalize_plugin_identifier(value, label).map_err(Into::into)
}

/// 市场插件名称的统一比较键；名称已由 normalized_identifier 限制为 ASCII。
pub fn marketplace_name_key(value: &str) -> String {
    value.to_ascii_lowercase()
}

/// 验证非空文本但不修改其展示内容。
fn non_empty<'a>(value: &'a str, label: &str) -> Result<&'a str> {
    if value.trim().is_empty() {
        return Err(ClaudePluginError::Invalid(format!("{label} 不能为空")));
    }
    Ok(value)
}

/// 验证 HTTP(S) URL，防止把未知 scheme 交给网络层。
fn validated_http_url(value: &str, label: &str) -> Result<String> {
    let value = non_empty(value, label)?;
    if !(value.starts_with("https://") || value.starts_with("http://")) {
        return Err(ClaudePluginError::Invalid(format!(
            "{label} 只允许 http 或 https：{value}"
        )));
    }
    Ok(value.to_owned())
}

/// 校验可选 npm/PyPI registry，只允许明确的 HTTP(S) 端点。
fn validate_optional_registry(registry: Option<&str>) -> Result<Option<String>> {
    registry
        .map(|value| validated_http_url(value, "registry URL"))
        .transpose()
}

/// 将 GitHub owner/repo 转换为 HTTPS Git 地址。
fn github_git_url(repo: &str) -> Result<String> {
    let repo = non_empty(repo, "GitHub repo")?;
    let mut parts = repo.split('/');
    let owner = parts.next();
    let name = parts.next();
    if owner.is_none() || name.is_none() || parts.next().is_some() {
        return Err(ClaudePluginError::Invalid(format!(
            "GitHub repo 必须为 owner/repo：{repo}"
        )));
    }
    Ok(format!("https://github.com/{repo}.git"))
}

/// 校验 Claude marketplace/plugin source 使用的固定 40 位提交 SHA。
fn validate_git_sha(sha: Option<&str>) -> Result<()> {
    let Some(sha) = sha else {
        return Ok(());
    };
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ClaudePluginError::Invalid(
            "Git source sha 必须是 40 位十六进制提交标识".to_owned(),
        ));
    }
    Ok(())
}

/// 合成 npm 包规范，保留 scope 包名中的 @。
fn package_spec(package: &str, version: Option<&str>) -> Result<String> {
    let package = non_empty(package, "npm/pip 包名")?;
    if package.contains(char::is_whitespace) {
        return Err(ClaudePluginError::Invalid(format!(
            "包名不能包含空白：{package}"
        )));
    }
    match version {
        Some(version) => Ok(format!("{package}@{}", non_empty(version, "包版本")?)),
        None => Ok(package.to_owned()),
    }
}

/// 构造 pip 的参数化包规范；pip 使用 `==` 锁定版本，不能复用 npm 的 `@version` 语法。
fn pip_package_spec(package: &str, version: Option<&str>) -> Result<String> {
    let package = non_empty(package, "pip 包名")?;
    if package.contains(char::is_whitespace)
        || package.contains(';')
        || package.contains('\n')
        || package.contains('\r')
    {
        return Err(ClaudePluginError::Invalid(format!(
            "pip 包名不能包含空白、环境标记或换行：{package}"
        )));
    }
    match version {
        Some(version) => {
            let version = non_empty(version, "pip 包版本")?;
            if version.contains(char::is_whitespace)
                || version.contains(';')
                || version.contains('\n')
                || version.contains('\r')
            {
                return Err(ClaudePluginError::Invalid(
                    "pip 包版本不能包含空白、环境标记或换行".to_owned(),
                ));
            }
            Ok(format!("{package}=={version}"))
        }
        None => Ok(package.to_owned()),
    }
}

/// 返回安全的相对路径；允许 Claude 市场惯用的 `./plugin`，拒绝 `..` 和绝对路径。
fn safe_relative_path(value: &str, label: &str) -> Result<PathBuf> {
    let value = non_empty(value, label)?;
    let path = Path::new(value);
    if !is_safe_relative_path(path) {
        return Err(ClaudePluginError::Invalid(format!(
            "{label} 必须是安全相对路径：{value}"
        )));
    }
    Ok(path.to_path_buf())
}

/// 将安全相对路径拼接到已授权根目录并确认不会跳出该根目录。
fn safe_relative_join(root: &Path, value: &str, label: &str) -> Result<PathBuf> {
    let relative = safe_relative_path(value, label)?;
    let root = fs::canonicalize(root)?;
    let joined = root.join(relative);
    if joined.exists() {
        let canonical = fs::canonicalize(&joined)?;
        if !canonical.starts_with(&root) {
            return Err(ClaudePluginError::Invalid(format!(
                "{label} 越出市场根目录：{value}"
            )));
        }
        Ok(canonical)
    } else {
        Ok(joined)
    }
}

/// 验证组件声明的每个路径都安全且不重复。
fn validate_component_paths(paths: &[String], label: &str) -> Result<()> {
    let mut unique = BTreeSet::new();
    for path in paths {
        let path = safe_relative_path(path, label)?;
        if !unique.insert(path) {
            return Err(ClaudePluginError::Invalid(format!("{label} 包含重复路径")));
        }
    }
    Ok(())
}

/// 检查 userConfig 值是否符合其声明类型和 enum 约束。
fn validate_user_config_value(
    name: &str,
    definition: &UserConfigDefinition,
    value: &Value,
) -> Result<()> {
    let values = if definition.multiple {
        value.as_array().ok_or_else(|| {
            ClaudePluginError::Invalid(format!("userConfig {name} 启用 multiple 时值必须为数组"))
        })?
    } else {
        std::slice::from_ref(value)
    };
    for value in values {
        let valid_type = match definition.value_type {
            UserConfigType::String | UserConfigType::Directory | UserConfigType::File => {
                value.is_string()
            }
            UserConfigType::Number => value.is_number(),
            UserConfigType::Boolean => value.is_boolean(),
            UserConfigType::Select => definition.enum_values.contains(value),
        };
        if !valid_type {
            return Err(ClaudePluginError::Invalid(format!(
                "userConfig {name} 值不符合 {:?} 类型或 enum 约束",
                definition.value_type
            )));
        }
        validate_user_config_bounds(name, definition, value)?;
    }
    Ok(())
}

/// 校验 number 数值范围和 string/file/directory 的长度范围。
fn validate_user_config_bounds(
    name: &str,
    definition: &UserConfigDefinition,
    value: &Value,
) -> Result<()> {
    let measured = match definition.value_type {
        UserConfigType::Number => value.as_f64(),
        UserConfigType::String | UserConfigType::Directory | UserConfigType::File => {
            value.as_str().map(|value| value.chars().count() as f64)
        }
        UserConfigType::Boolean | UserConfigType::Select => None,
    };
    if let Some(measured) = measured
        && (definition.min.is_some_and(|minimum| measured < minimum)
            || definition.max.is_some_and(|maximum| measured > maximum))
    {
        return Err(ClaudePluginError::Invalid(format!(
            "userConfig {name} 超出 min/max 约束"
        )));
    }
    Ok(())
}

/// 规范化并确认一个插件根目录确实存在插件清单。
fn canonical_plugin_root(root: &Path) -> Result<PathBuf> {
    let root = fs::canonicalize(root)?;
    if !root.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "插件根目录不是目录：{}",
            root.display()
        )));
    }
    let manifest = root.join(CLAUDE_PLUGIN_MANIFEST);
    if !manifest.is_file() {
        return Err(ClaudePluginError::Invalid(format!(
            "插件根目录缺少 {}：{}",
            CLAUDE_PLUGIN_MANIFEST,
            root.display()
        )));
    }
    Ok(root)
}

/// 从插件根目录扫描声明的 Markdown 文件；目录递归、文件直接纳入。
fn scan_components(root: &Path, declarations: &[String]) -> Result<Vec<ComponentFile>> {
    let mut files = BTreeSet::new();
    for declaration in declarations {
        let path = safe_relative_join(root, declaration, "组件路径")?;
        if path.is_file() {
            insert_markdown_file(root, &path, &mut files)?;
        } else if path.is_dir() {
            scan_component_directory(root, &path, &mut files)?;
        } else {
            return Err(ClaudePluginError::Invalid(format!(
                "组件路径不存在：{}",
                path.display()
            )));
        }
    }
    Ok(files
        .into_iter()
        .map(|path| ComponentFile {
            relative_path: path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
            path,
        })
        .collect())
}

/// 扫描清单显式声明的组件；未声明时遵循 Claude Code 的默认目录约定。
fn scan_declared_or_default_components(
    root: &Path,
    declarations: &[String],
    default_directory: &str,
) -> Result<Vec<ComponentFile>> {
    if !declarations.is_empty() {
        return scan_components(root, declarations);
    }
    let default_path = root.join(default_directory);
    if default_path.is_dir() {
        scan_components(root, &[default_directory.to_owned()])
    } else {
        Ok(Vec::new())
    }
}

/// 加载 inline hooks、清单引用的相对 hook 文件，或默认 `hooks/hooks.json`。
fn load_hooks(
    root: &Path,
    declaration: Option<&Value>,
    variables: &BTreeMap<String, String>,
) -> Result<Option<Value>> {
    let declarations = match declaration {
        Some(value) => vec![value.clone()],
        None => {
            let default_path = root.join("hooks/hooks.json");
            if !default_path.is_file() {
                return Ok(None);
            }
            vec![Value::String("hooks/hooks.json".to_owned())]
        }
    };
    let mut events = Map::new();
    for declaration in declarations {
        merge_hook_declaration(root, declaration, variables, &mut events)?;
    }
    if events.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Value::Object(events)))
    }
}

/// 递归展开 Claude Hook 的包装对象、路径数组和 hooks/hooks.json 文件。
fn merge_hook_declaration(
    root: &Path,
    declaration: Value,
    variables: &BTreeMap<String, String>,
    events: &mut Map<String, Value>,
) -> Result<()> {
    match declaration {
        Value::String(path) => {
            let value = load_hook_file(root, &path)?;
            merge_hook_declaration(root, value, variables, events)
        }
        Value::Array(values) => {
            for value in values {
                merge_hook_declaration(root, value, variables, events)?;
            }
            Ok(())
        }
        Value::Object(object) => {
            // hooks/hooks.json 和 plugin.json 的标准格式都可能包一层
            // `{ "hooks": { "PreToolUse": [...] } }`；外层 description 等字段忽略。
            if let Some(inner) = object.get("hooks") {
                return merge_hook_declaration(root, inner.clone(), variables, events);
            }
            let value = interpolate_json(&Value::Object(object), variables)?;
            let Some(object) = value.as_object() else {
                return Err(ClaudePluginError::Invalid(
                    "Claude Hooks 声明必须是对象".to_owned(),
                ));
            };
            for (event, groups) in object {
                merge_hook_event(events, event, groups.clone());
            }
            Ok(())
        }
        _ => Err(ClaudePluginError::Invalid(
            "Claude Hooks 声明必须是对象、路径或数组".to_owned(),
        )),
    }
}

/// 合并多个 Hook 文件中同一事件的 matcher 数组，保持声明顺序。
fn merge_hook_event(events: &mut Map<String, Value>, event: &str, value: Value) {
    let Some(existing) = events.get_mut(event) else {
        events.insert(event.to_owned(), value);
        return;
    };
    let mut merged = match existing.take() {
        Value::Array(values) => values,
        value => vec![value],
    };
    match value {
        Value::Array(values) => merged.extend(values),
        value => merged.push(value),
    }
    *existing = Value::Array(merged);
}

/// 在插件根目录内读取清单声明的 hook JSON 文件。
fn load_hook_file(root: &Path, declaration: &str) -> Result<Value> {
    let path = safe_relative_join(root, declaration, "hooks 文件")?;
    if !path.is_file() {
        return Err(ClaudePluginError::Invalid(format!(
            "hooks 文件不存在：{}",
            path.display()
        )));
    }
    Ok(serde_json::from_slice(&read_limited(&path)?)?)
}

/// 将配置名转换为可由 `${CLAUDE_PLUGIN_*}` 引用的稳定变量名。
fn normalize_variable_name(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() {
                byte.to_ascii_uppercase() as char
            } else {
                '_'
            }
        })
        .collect()
}

/// 将标量或多选 userConfig 转换为环境变量值，复合 JSON 不会被隐式字符串化。
fn config_value_as_variable(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Array(values) => values
            .iter()
            .map(config_value_as_variable)
            .collect::<Option<Vec<_>>>()
            .map(|values| values.join(",")),
        Value::Null | Value::Object(_) => None,
    }
}

/// 深度优先扫描目录中的 Markdown 组件；组件目录内不跟随任何符号链接。
fn scan_component_directory(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(ClaudePluginError::Invalid(format!(
                "组件目录不允许符号链接：{}",
                path.display()
            )));
        } else if metadata.is_dir() {
            scan_component_directory(root, &path, files)?;
        } else if metadata.is_file() {
            insert_markdown_file(root, &path, files)?;
        }
    }
    Ok(())
}

/// 仅纳入 `.md` 文件，并二次确认文件仍在插件根目录内。
fn insert_markdown_file(root: &Path, path: &Path, files: &mut BTreeSet<PathBuf>) -> Result<()> {
    let path = fs::canonicalize(path)?;
    if !path.starts_with(root) {
        return Err(ClaudePluginError::Invalid(format!(
            "组件文件越出插件根目录：{}",
            path.display()
        )));
    }
    if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
        files.insert(path);
    }
    Ok(())
}

/// 读取可选 `.mcp.json` 并返回 `mcpServers` 映射。
fn load_mcp_file(root: &Path) -> Result<Option<BTreeMap<String, Value>>> {
    let path = root.join(".mcp.json");
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(parse_mcp_servers_value(
        serde_json::from_slice(&read_limited(&path)?)?,
        &path,
    )?))
}

/// 把插件声明的 DXT/MCPB `manifest.user_config` 合并为 Claude 插件 userConfig。
///
/// Claude Code 允许插件通过 MCP bundle 声明配置；这些字段必须进入同一个
/// SecretStore/热刷新管道，否则设置界面只能看到顶层 plugin.json 的字段。
fn merge_mcp_bundle_user_config(root: &Path, manifest: &mut PluginManifest) -> Result<()> {
    let declarations = manifest.mcp_servers.files.clone();
    for declaration in declarations {
        let lower = declaration.to_ascii_lowercase();
        if !lower.ends_with(".mcpb") && !lower.ends_with(".dxt") {
            continue;
        }
        let (_extracted, bundle_manifest) = materialize_mcp_bundle(root, &declaration)?;
        let object = bundle_manifest.as_object().ok_or_else(|| {
            ClaudePluginError::Invalid("MCPB/DXT manifest 顶层必须是对象".to_owned())
        })?;
        let Some(schema) = object
            .get("user_config")
            .or_else(|| object.get("userConfig"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for (name, definition) in schema {
            normalized_identifier(name, "MCPB/DXT userConfig 字段")?;
            let parsed: UserConfigDefinition =
                serde_json::from_value(definition.clone()).map_err(|error| {
                    ClaudePluginError::Invalid(format!(
                        "MCPB/DXT userConfig {name} 定义无效：{error}"
                    ))
                })?;
            // plugin.json 显式声明优先，避免 bundle 与插件作者定义冲突时
            // 静默改变已有字段的敏感性或类型。
            manifest.user_config.entry(name.clone()).or_insert(parsed);
        }
    }
    Ok(())
}

/// 远程 MCPB/DXT 的原始归档缓存路径；键只由完整 URL 的 SHA-256 决定。
fn mcp_bundle_url_cache_path(root: &Path, url: &str) -> PathBuf {
    let digest = Sha256::digest(url.as_bytes());
    root.join(".mcpb-cache")
        .join("archives")
        .join(format!("{digest:x}"))
}

/// 确认一个缓存目录是普通目录，并且 canonical 路径仍位于指定根目录内。
fn ensure_mcp_bundle_directory(path: &Path, root: &Path, label: &str) -> Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Err(error) = fs::create_dir(path)
                && error.kind() != io::ErrorKind::AlreadyExists
            {
                return Err(error.into());
            }
        }
        Err(error) => return Err(error.into()),
    }
    // 创建后重新读取 metadata，覆盖并发创建或路径被替换为符号链接的情况。
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(ClaudePluginError::Invalid(format!(
            "{}不允许是符号链接：{}",
            label,
            path.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "{}不是目录：{}",
            label,
            path.display()
        )));
    }
    let canonical = fs::canonicalize(path)?;
    if !canonical.starts_with(root) {
        return Err(ClaudePluginError::Invalid(format!(
            "{}越出插件根目录：{}",
            label,
            path.display()
        )));
    }
    Ok(canonical)
}

/// 为 MCPB/DXT 缓存建立固定的安全布局：原始归档与解包内容相互隔离。
fn secure_mcp_bundle_cache(root: &Path) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let root = fs::canonicalize(root)?;
    let metadata = fs::symlink_metadata(&root)?;
    if !metadata.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "MCPB/DXT 插件根目录不是目录：{}",
            root.display()
        )));
    }
    let cache_root =
        ensure_mcp_bundle_directory(&root.join(".mcpb-cache"), &root, "MCPB/DXT 缓存根目录")?;
    let archives = ensure_mcp_bundle_directory(
        &cache_root.join("archives"),
        &root,
        "MCPB/DXT 原始归档缓存目录",
    )?;
    let extracted = ensure_mcp_bundle_directory(
        &cache_root.join("extracted"),
        &root,
        "MCPB/DXT 解包缓存目录",
    )?;
    Ok((cache_root, archives, extracted))
}

/// 用内容 SHA-256 生成单一、稳定的解包目录名。
fn mcp_bundle_content_cache_name(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 从流中读取归档并限制最大字节数；响应使用 chunked 编码时也不能绕过限制。
fn read_mcp_bundle_reader<R: Read>(reader: &mut R) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_MCPB_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ClaudePluginError::Invalid(format!("读取 MCPB/DXT 失败：{error}")))?;
    if bytes.len() > MAX_MCPB_BYTES {
        return Err(ClaudePluginError::Invalid(format!(
            "MCPB/DXT 超过 {} MB 限制",
            MAX_MCPB_BYTES / (1024 * 1024)
        )));
    }
    Ok(bytes)
}

/// 读取本地归档或远程 URL 缓存，并拒绝符号链接和超大文件。
fn read_mcp_bundle_file(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(ClaudePluginError::Invalid(format!(
            "不允许读取符号链接 MCPB/DXT：{}",
            path.display()
        )));
    }
    if !metadata.is_file() {
        return Err(ClaudePluginError::Invalid(format!(
            "MCPB/DXT 不是普通文件：{}",
            path.display()
        )));
    }
    if metadata.len() > MAX_MCPB_BYTES as u64 {
        return Err(ClaudePluginError::Invalid(format!(
            "MCPB/DXT 超过 {} MB 限制：{}",
            MAX_MCPB_BYTES / (1024 * 1024),
            path.display()
        )));
    }
    let mut file = fs::File::open(path)?;
    read_mcp_bundle_reader(&mut file)
}

/// 读取远程归档缓存；缓存文件不存在时由调用方负责下载并原子写入。
fn read_mcp_bundle_cache(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(_) => read_mcp_bundle_file(path).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// 仅信任由完整解包最后写入完成标记的内容缓存目录。
fn read_completed_mcp_bundle(extracted: &Path, content_hash: &str) -> Result<Option<Value>> {
    let metadata = match fs::symlink_metadata(extracted) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(ClaudePluginError::Invalid(format!(
            "MCPB/DXT 解包缓存目录不允许是符号链接：{}",
            extracted.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "MCPB/DXT 解包缓存路径不是目录：{}",
            extracted.display()
        )));
    }
    let marker = extracted.join(MCPB_COMPLETION_MARKER);
    let Ok(marker_bytes) = read_limited(&marker) else {
        return Ok(None);
    };
    if marker_bytes != content_hash.as_bytes() {
        return Ok(None);
    }
    let manifest =
        serde_json::from_slice::<Value>(&read_limited(&extracted.join("manifest.json"))?)?;
    if !manifest.is_object() {
        return Ok(None);
    }
    Ok(Some(manifest))
}

/// 读取并缓存本地或远程 MCPB/DXT 归档，返回解包目录和 `manifest.json`。
fn materialize_mcp_bundle(root: &Path, declaration: &str) -> Result<(PathBuf, Value)> {
    let (_cache_root, archives_root, extracted_root) = secure_mcp_bundle_cache(root)?;
    let root = fs::canonicalize(root)?;
    let bytes = if declaration.starts_with("http://") || declaration.starts_with("https://") {
        let cache_path = archives_root.join(
            mcp_bundle_url_cache_path(&root, declaration)
                .file_name()
                .ok_or_else(|| {
                    ClaudePluginError::Invalid("MCPB/DXT URL 缓存文件名无效".to_owned())
                })?,
        );
        if let Some(bytes) = read_mcp_bundle_cache(&cache_path)? {
            bytes
        } else {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .map_err(|error| {
                    ClaudePluginError::Invalid(format!("MCPB HTTP 客户端创建失败：{error}"))
                })?;
            let response = client
                .get(declaration)
                .header(reqwest::header::USER_AGENT, "KeenCode-Claude-Plugin/1")
                .send()
                .map_err(|error| {
                    ClaudePluginError::Invalid(format!("下载 MCPB/DXT 失败：{error}"))
                })?
                .error_for_status()
                .map_err(|error| {
                    ClaudePluginError::Invalid(format!("下载 MCPB/DXT 返回错误：{error}"))
                })?;
            let bytes = match read_http_response_limited(response, MAX_MCPB_BYTES) {
                Ok(bytes) => bytes,
                Err(HttpResponseReadError::TooLarge { .. }) => {
                    return Err(ClaudePluginError::Invalid(format!(
                        "MCPB/DXT 超过 {} MB 限制",
                        MAX_MCPB_BYTES / (1024 * 1024)
                    )));
                }
                Err(HttpResponseReadError::Read(error)) => {
                    return Err(ClaudePluginError::Invalid(format!(
                        "读取 MCPB/DXT 失败：{error}"
                    )));
                }
            };
            // atomic_write_private 使用同目录临时文件和原子替换；下载或写入失败时
            // 不会留下半个归档，后续调用仍可重新取得完整内容。
            crate::storage::atomic_write_private(&cache_path, &bytes).map_err(|error| {
                ClaudePluginError::Invalid(format!("写入 MCPB/DXT 本地缓存失败：{}", error))
            })?;
            bytes
        }
    } else {
        let relative = safe_relative_path(declaration, "MCPB/DXT 文件")?;
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(ClaudePluginError::Invalid(format!(
                "不允许读取符号链接 MCPB/DXT：{}",
                path.display()
            )));
        }
        let canonical = fs::canonicalize(&path)?;
        if !canonical.starts_with(&root) {
            return Err(ClaudePluginError::Invalid(format!(
                "MCPB/DXT 文件越出插件根目录：{}",
                path.display()
            )));
        }
        read_mcp_bundle_file(&path)?
    };

    let content_hash = mcp_bundle_content_cache_name(&bytes);
    let extracted = extracted_root.join(&content_hash);
    let _extraction_guard = MCPB_EXTRACTION_LOCK
        .lock()
        .map_err(|_| ClaudePluginError::Invalid("MCPB/DXT 解包缓存锁已损坏".to_owned()))?;
    if let Some(manifest) = read_completed_mcp_bundle(&extracted, &content_hash)? {
        return Ok((extracted, manifest));
    }
    // 旧实现留下的目录、失败解包或缺少完成标记的目录都不能作为缓存命中。
    // 该路径由 SHA-256 和受控 extracted 根构造，不接受调用方路径。
    if let Err(error) = fs::remove_dir_all(&extracted)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error.into());
    }

    // 所有归档条目先写入唯一临时目录，只有完整解包、路径/大小校验和
    // manifest JSON 校验全部通过后，才以同一文件系统上的 rename 提交。
    // 因此任意失败都不会在内容哈希目录留下 manifest，后续调用不会误判为完成。
    let temporary = tempfile::Builder::new()
        .prefix(".extracting-")
        .tempdir_in(&extracted_root)?;
    let temporary_path = temporary.path().to_path_buf();
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| ClaudePluginError::Invalid(format!("MCPB/DXT ZIP 无效：{error}")))?;
    if archive.len() > MAX_MCPB_ENTRIES {
        return Err(ClaudePluginError::Invalid(
            "MCPB/DXT 文件数量超过限制".to_owned(),
        ));
    }
    let mut extracted_bytes = 0u64;
    let mut paths = BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            ClaudePluginError::Invalid(format!("读取 MCPB/DXT 条目失败：{error}"))
        })?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| {
                ClaudePluginError::Invalid(format!("MCPB/DXT 条目路径越界：{}", entry.name()))
            })?
            .to_path_buf();
        if !paths.insert(relative.clone()) {
            return Err(ClaudePluginError::Invalid(format!(
                "MCPB/DXT 包含重复条目：{}",
                relative.display()
            )));
        }
        let destination = temporary_path.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&destination)?;
            continue;
        }
        extracted_bytes = extracted_bytes
            .checked_add(entry.size())
            .ok_or_else(|| ClaudePluginError::Invalid("MCPB/DXT 解包大小溢出".to_owned()))?;
        if extracted_bytes > MAX_MCPB_EXTRACTED_BYTES {
            return Err(ClaudePluginError::Invalid(
                "MCPB/DXT 解包后超过磁盘保护上限".to_owned(),
            ));
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = fs::File::create(&destination)?;
        io::copy(&mut entry, &mut output)?;
        output.sync_all()?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&destination, fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    let manifest_path = temporary_path.join("manifest.json");
    let manifest = serde_json::from_slice::<Value>(&read_limited(&manifest_path)?)?;
    if !manifest.is_object() {
        return Err(ClaudePluginError::Invalid(
            "MCPB/DXT manifest 顶层必须是对象".to_owned(),
        ));
    }
    let marker_path = temporary_path.join(MCPB_COMPLETION_MARKER);
    // 保持创建时的可写句柄完成落盘；Windows 对只读 `File::open` 句柄调用
    // `FlushFileBuffers` 会返回 Access Denied，导致完整归档永远无法提交缓存。
    let mut marker = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&marker_path)?;
    std::io::Write::write_all(&mut marker, content_hash.as_bytes())?;
    marker.sync_all()?;
    drop(marker);

    if let Err(error) = fs::rename(&temporary_path, &extracted) {
        // 另一个并发调用可能已经提交了完全相同的内容缓存。只有完成标记与
        // 内容哈希一致时才复用；TempDir 会清理本次未提交目录。
        if let Some(existing_manifest) = read_completed_mcp_bundle(&extracted, &content_hash)? {
            return Ok((extracted, existing_manifest));
        }
        return Err(ClaudePluginError::Invalid(format!(
            "提交 MCPB/DXT 解包缓存失败（{} -> {}）：{error}",
            temporary_path.display(),
            extracted.display()
        )));
    }
    Ok((extracted, manifest))
}

/// 将 DXT/MCPB `manifest.json` 的 server 描述转换成 Peri MCP 配置。
fn mcp_bundle_servers(extracted: &Path, manifest: Value) -> Result<BTreeMap<String, Value>> {
    let object = manifest
        .as_object()
        .ok_or_else(|| ClaudePluginError::Invalid("MCPB/DXT manifest 顶层必须是对象".to_owned()))?;
    let server = object
        .get("server")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ClaudePluginError::Invalid("MCPB/DXT manifest 缺少 server 配置".to_owned())
        })?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("mcpb-server")
        .to_owned();
    let mut config = server
        .get("mcp_config")
        .or_else(|| server.get("mcpConfig"))
        .cloned()
        .unwrap_or(Value::Null);
    if config.is_null() {
        let entry_point = server
            .get("entry_point")
            .or_else(|| server.get("entryPoint"))
            .and_then(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .ok_or_else(|| {
                ClaudePluginError::Invalid("MCPB/DXT server 缺少 entry_point".to_owned())
            })?;
        let server_type = server
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("binary")
            .to_ascii_lowercase();
        let entry = extracted.join(safe_relative_path(entry_point, "MCPB entry_point")?);
        let (command, default_args) = match server_type.as_str() {
            "node" | "javascript" => ("node".to_owned(), vec![entry.display().to_string()]),
            "python" | "python3" => ("python3".to_owned(), vec![entry.display().to_string()]),
            "binary" | "executable" => (entry.display().to_string(), Vec::new()),
            other => {
                return Err(ClaudePluginError::Invalid(format!(
                    "不支持的 MCPB server.type：{other}"
                )));
            }
        };
        let args = server
            .get("args")
            .cloned()
            .unwrap_or_else(|| Value::Array(default_args.into_iter().map(Value::String).collect()));
        let mut map = Map::new();
        map.insert("command".to_owned(), Value::String(command));
        map.insert("args".to_owned(), args);
        if let Some(env) = server.get("env") {
            map.insert("env".to_owned(), env.clone());
        }
        config = Value::Object(map);
    }
    let config = replace_mcp_bundle_dir(&config, extracted)?;
    let mut servers = BTreeMap::new();
    servers.insert(name, normalize_mcp_server_value(config)?);
    Ok(servers)
}

/// 替换 DXT 专用 `${__dirname}` 变量，其他变量留给统一插值阶段。
fn replace_mcp_bundle_dir(value: &Value, extracted: &Path) -> Result<Value> {
    match value {
        Value::String(text) => Ok(Value::String(
            text.replace("${__dirname}", &extracted.display().to_string()),
        )),
        Value::Array(values) => values
            .iter()
            .map(|value| replace_mcp_bundle_dir(value, extracted))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), replace_mcp_bundle_dir(value, extracted)?)))
            .collect::<Result<Map<_, _>>>()
            .map(Value::Object),
        value => Ok(value.clone()),
    }
}

/// 从清单引用的相对 MCP 文件读取 Server；路径受插件根目录边界保护。
fn load_mcp_servers_file(root: &Path, declaration: &str) -> Result<BTreeMap<String, Value>> {
    if declaration.starts_with("http://") || declaration.starts_with("https://") {
        let (extracted, manifest) = materialize_mcp_bundle(root, declaration)?;
        return mcp_bundle_servers(&extracted, manifest);
    }
    let path = safe_relative_join(root, declaration, "mcpServers 文件")?;
    if !path.is_file() {
        return Err(ClaudePluginError::Invalid(format!(
            "mcpServers 文件不存在：{}",
            path.display()
        )));
    }
    if matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("mcpb" | "dxt")
    ) {
        let (extracted, manifest) = materialize_mcp_bundle(root, declaration)?;
        return mcp_bundle_servers(&extracted, manifest);
    }
    parse_mcp_servers_value(serde_json::from_slice(&read_limited(&path)?)?, &path)
}

/// 从 `.mcp.json`、mcpServers 文件或内联 JSON 提取对象/数组形式的 Server 映射。
fn parse_mcp_servers_value(value: Value, path: &Path) -> Result<BTreeMap<String, Value>> {
    let object = value.as_object().ok_or_else(|| {
        ClaudePluginError::Invalid(format!("MCP 配置顶层必须是对象：{}", path.display()))
    })?;
    let servers = object
        .get("mcpServers")
        .or_else(|| object.get("servers"))
        .cloned()
        .unwrap_or_else(|| Value::Object(object.clone()));
    parse_mcp_servers_entries(servers)
}

/// 解析 `mcpServers` 的对象和数组形式；数组项可为 `{ name, config }` 或 `{ name, command }`。
fn parse_mcp_servers_entries(value: Value) -> Result<BTreeMap<String, Value>> {
    let mut servers = BTreeMap::new();
    match value {
        Value::Object(values) => {
            for (name, value) in values {
                insert_mcp_server(&mut servers, name, value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                let mut entry = value.as_object().cloned().ok_or_else(|| {
                    ClaudePluginError::Invalid("mcpServers 数组项必须是对象".to_owned())
                })?;
                let name = entry
                    .remove("name")
                    .and_then(|value| value.as_str().map(ToOwned::to_owned))
                    .ok_or_else(|| {
                        ClaudePluginError::Invalid("mcpServers 数组项缺少 name".to_owned())
                    })?;
                let config = entry
                    .remove("config")
                    .unwrap_or_else(|| Value::Object(entry));
                insert_mcp_server(&mut servers, name, config)?;
            }
        }
        _ => {
            return Err(ClaudePluginError::Invalid(
                "mcpServers 必须是对象或数组".to_owned(),
            ));
        }
    }
    Ok(servers)
}

/// 插入唯一的 MCP Server；同一 JSON 形式内的重复名称是配置错误。
fn insert_mcp_server(
    servers: &mut BTreeMap<String, Value>,
    name: String,
    config: Value,
) -> Result<()> {
    let name = normalized_identifier(&name, "MCP Server 名称")?;
    if servers.insert(name.clone(), config).is_some() {
        return Err(ClaudePluginError::Invalid(format!(
            "MCP Server 名称重复：{name}"
        )));
    }
    Ok(())
}

/// 将 Claude MCP Server 的 `type` 判别字段归一为 Peri 当前支持的 command/url。
fn normalize_mcp_server_value(value: Value) -> Result<Value> {
    let mut object = value
        .as_object()
        .cloned()
        .ok_or_else(|| ClaudePluginError::Invalid("MCP Server 配置必须是对象".to_owned()))?;
    if let Some(kind) = object
        .remove("type")
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
    {
        match kind.as_str() {
            "stdio" | "streamable-http" | "http" => {}
            "sse" | "websocket" | "sdk" | "claudeai-proxy" => {
                return Err(ClaudePluginError::Invalid(format!(
                    "当前运行时不支持该 MCP transport type={kind}"
                )));
            }
            other => {
                return Err(ClaudePluginError::Invalid(format!(
                    "未知 Claude MCP transport type={other}"
                )));
            }
        }
    }
    if object.get("disabled") == Some(&Value::Bool(false)) {
        object.remove("disabled");
    }
    if object.get("serverUrl").is_some()
        && object.get("url").is_none()
        && let Some(url) = object.remove("serverUrl")
    {
        object.insert("url".to_owned(), url);
    }
    Ok(Value::Object(object))
}

/// 计算插件来源目录的确定性内容指纹。
///
/// 文件名、目录结构和文件内容都会参与摘要；符号链接只参与摘要校验，后续复制
/// 阶段会统一拒绝。这样同一版本的来源发生代码变化时，安装缓存路径也会变化。
fn plugin_tree_fingerprint(root: &Path) -> Result<String> {
    let root = canonical_plugin_root(root)?;
    let mut hasher = Sha256::new();
    fingerprint_plugin_tree(&root, &root, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn fingerprint_plugin_tree(root: &Path, current: &Path, hasher: &mut Sha256) -> Result<()> {
    let mut entries = fs::read_dir(current)?.collect::<std::result::Result<Vec<_>, io::Error>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(|error| {
            ClaudePluginError::Invalid(format!("插件路径无法生成摘要：{error}"))
        })?;
        if relative
            .components()
            .any(|component| component.as_os_str() == ".git")
        {
            continue;
        }
        hasher.update(b"path\0");
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0_u8]);

        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            let target = resolve_internal_file_symlink(root, &path)?;
            hasher.update(b"symlink\0");
            hasher.update(
                target
                    .strip_prefix(root)
                    .map_err(|error| {
                        ClaudePluginError::Invalid(format!("插件符号链接目标无效：{error}"))
                    })?
                    .to_string_lossy()
                    .as_bytes(),
            );
            hasher.update([0_u8]);
        } else if metadata.is_dir() {
            hasher.update(b"directory\0");
            fingerprint_plugin_tree(root, &path, hasher)?;
        } else if metadata.is_file() {
            hasher.update(b"file\0");
            let mut file = fs::File::open(&path)?;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            hasher.update([0_u8]);
        } else {
            return Err(ClaudePluginError::Invalid(format!(
                "插件目录包含不支持的文件类型：{}",
                path.display()
            )));
        }
    }
    Ok(())
}

/// 安全递归复制插件树；符号链接按目标校验后复制其文件内容，不保留外部链接。
fn copy_plugin_tree(source: &Path, destination: &Path) -> Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(ClaudePluginError::Invalid(format!(
                "插件缓存目标不允许是符号链接：{}",
                destination.display()
            )));
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let temporary = destination.with_extension("installing");
    match fs::symlink_metadata(&temporary) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(ClaudePluginError::Invalid(format!(
                "插件缓存临时目标不允许是符号链接：{}",
                temporary.display()
            )));
        }
        Ok(_) => fs::remove_dir_all(&temporary)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = destination
        .parent()
        .ok_or_else(|| ClaudePluginError::Invalid("插件缓存目标缺少父目录".to_owned()))?;
    let cache_root = parent
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| ClaudePluginError::Invalid("插件缓存目标缺少缓存根目录".to_owned()))?;
    ensure_controlled_descendant_chain(cache_root, parent, "插件缓存目标父目录")?;
    let source_metadata = fs::symlink_metadata(source)?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "插件来源根目录必须是普通目录：{}",
            source.display()
        )));
    }
    let source = fs::canonicalize(source)?;
    fs::create_dir(&temporary)?;
    if let Err(error) = copy_tree_entry(&source, &source, &temporary) {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, destination) {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error.into());
    }
    Ok(())
}

/// 清理批量安装失败时本次新建的缓存目录；已有版本缓存不受影响。
fn cleanup_copied_plugins(destinations: &[PathBuf]) {
    for destination in destinations {
        let Some(cache_root) = destination
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
        else {
            tracing::warn!(path = %destination.display(), "插件缓存清理路径缺少缓存根目录");
            continue;
        };
        if let Err(error) = validate_controlled_path(cache_root, destination, "插件缓存清理路径")
        {
            tracing::warn!(
                path = %destination.display(),
                %error,
                "插件缓存清理路径不安全，跳过删除"
            );
            continue;
        }
        match fs::symlink_metadata(destination) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                tracing::warn!(
                    path = %destination.display(),
                    "跳过符号链接插件缓存清理"
                );
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!(path = %destination.display(), %error, "检查插件缓存清理目标失败");
                continue;
            }
            Ok(_) => {}
        }
        if let Err(error) = fs::remove_dir_all(destination)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %destination.display(),
                %error,
                "批量插件安装失败后的缓存清理失败"
            );
        }
    }
}

/// 状态提交成功后清理不再被任何安装记录引用的旧缓存。
///
/// 只删除当前缓存根目录内、且曾经出现在旧状态中的路径；用户市场目录和
/// 其他下载目录不会被此函数触碰。清理失败不回滚已提交状态，只记录警告。
fn cleanup_unreferenced_plugin_caches(
    storage: &PluginStorage,
    previous: &PluginState,
    next: &PluginState,
) {
    if let Err(error) = storage.validate_layout() {
        tracing::warn!(%error, "插件缓存布局不安全，跳过旧缓存清理");
        return;
    }
    let referenced = next
        .plugins
        .iter()
        .map(|plugin| plugin.install_path.clone())
        .collect::<BTreeSet<_>>();
    for plugin in &previous.plugins {
        let path = &plugin.install_path;
        if referenced.contains(path) || !is_versioned_plugin_cache_path(storage, path) {
            continue;
        }
        if let Err(error) = validate_controlled_path(&storage.cache_root, path, "插件缓存清理路径")
        {
            tracing::warn!(path = %path.display(), %error, "插件缓存清理路径不安全，跳过删除");
            continue;
        }
        if let Ok(metadata) = fs::symlink_metadata(path)
            && metadata.file_type().is_symlink()
        {
            tracing::warn!(path = %path.display(), "跳过符号链接插件缓存清理");
            continue;
        }
        if let Err(error) = fs::remove_dir_all(path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %path.display(),
                %error,
                "清理旧版 Claude 插件缓存失败"
            );
        }
    }
}

fn is_versioned_plugin_cache_path(storage: &PluginStorage, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(&storage.cache_root) else {
        return false;
    };
    let mut components = relative.components();
    matches!(
        (
            components.next(),
            components.next(),
            components.next(),
            components.next(),
        ),
        (
            Some(Component::Normal(_)),
            Some(Component::Normal(_)),
            Some(Component::Normal(_)),
            None
        )
    )
}

/// 解析插件根内的普通文件链接；越界、目录或特殊文件链接均拒绝。
pub(crate) fn resolve_internal_file_symlink(root: &Path, path: &Path) -> Result<PathBuf> {
    let target = fs::canonicalize(path)?;
    if !target.starts_with(root) {
        return Err(ClaudePluginError::Invalid(format!(
            "插件符号链接越出根目录：{}",
            path.display()
        )));
    }
    if !target.is_file() {
        return Err(ClaudePluginError::Invalid(format!(
            "插件符号链接目标必须是普通文件：{}",
            path.display()
        )));
    }
    Ok(target)
}

/// 安全复制目录树；根内文件链接解引用为普通文件，不保留链接。
fn copy_tree_entry(root: &Path, source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            fs::copy(
                resolve_internal_file_symlink(root, &source_path)?,
                destination_path,
            )?;
        } else if metadata.is_dir() {
            fs::create_dir_all(&destination_path)?;
            copy_tree_entry(root, &source_path, &destination_path)?;
        } else if metadata.is_file() {
            fs::copy(source_path, destination_path)?;
        } else {
            return Err(ClaudePluginError::Invalid(format!(
                "插件目录包含不支持的文件类型：{}",
                source_path.display()
            )));
        }
    }
    Ok(())
}

/// 确认插件缓存根目录是当前存储布局中的真实目录。
fn canonical_plugin_cache_root(storage: &PluginStorage) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(&storage.cache_root)?;
    if metadata.file_type().is_symlink() {
        return Err(ClaudePluginError::Invalid(format!(
            "插件缓存根目录不允许是符号链接：{}",
            storage.cache_root.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(ClaudePluginError::Invalid(format!(
            "插件缓存根目录不是目录：{}",
            storage.cache_root.display()
        )));
    }
    fs::canonicalize(&storage.cache_root).map_err(Into::into)
}

/// 内容指纹必须是唯一的一层、全小写的 SHA-256 十六进制目录名。
fn is_content_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// 确保公开状态不存在重复 ID、敏感值或指向外部的安装路径。
fn validate_state(storage: &PluginStorage, state: &PluginState) -> Result<()> {
    storage.validate_layout()?;
    let mut ids = BTreeSet::new();
    let cache_root = if state.plugins.is_empty() {
        None
    } else {
        Some(canonical_plugin_cache_root(storage)?)
    };
    for plugin in &state.plugins {
        let id = require_marketplace_id(&plugin.id)?;
        let key = format!(
            "{}@{}",
            marketplace_name_key(id.marketplace.as_deref().unwrap_or_default()),
            marketplace_name_key(&id.plugin)
        );
        if !ids.insert(key) {
            return Err(ClaudePluginError::Invalid(format!(
                "插件状态包含重复 ID：{id}"
            )));
        }
        non_empty(&plugin.version, "已安装插件版本")?;
        if !plugin.install_path.is_absolute() {
            return Err(ClaudePluginError::Invalid(format!(
                "安装路径必须为绝对路径：{id}"
            )));
        }
        let fingerprint = plugin
            .install_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| ClaudePluginError::Invalid(format!("安装路径缺少内容指纹：{id}")))?;
        if !is_content_fingerprint(fingerprint) {
            return Err(ClaudePluginError::Invalid(format!(
                "安装路径内容指纹无效：{id}"
            )));
        }
        let expected = storage.versioned_path(&id, fingerprint)?;
        if plugin.install_path != expected {
            return Err(ClaudePluginError::Invalid(format!(
                "安装路径必须是当前缓存根目录下稳定的小写路径：{id}"
            )));
        }
        let relative = plugin
            .install_path
            .strip_prefix(&storage.cache_root)
            .map_err(|_| {
                ClaudePluginError::Invalid(format!("安装路径不在当前缓存根目录内：{id}"))
            })?;
        let mut components = relative.components();
        if !matches!(
            (
                components.next(),
                components.next(),
                components.next(),
                components.next(),
            ),
            (
                Some(Component::Normal(_)),
                Some(Component::Normal(_)),
                Some(Component::Normal(_)),
                None
            )
        ) {
            return Err(ClaudePluginError::Invalid(format!(
                "安装路径必须只有市场、插件和一个内容指纹层：{id}"
            )));
        }
        validate_controlled_path(&storage.cache_root, &plugin.install_path, "插件安装路径")?;
        let metadata = fs::symlink_metadata(&plugin.install_path).map_err(|error| {
            ClaudePluginError::Invalid(format!("插件缓存目录不可用：{id}：{error}"))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ClaudePluginError::Invalid(format!(
                "插件安装路径必须是缓存根目录内的普通目录：{id}"
            )));
        }
        let canonical_path = fs::canonicalize(&plugin.install_path)?;
        if !canonical_path.starts_with(cache_root.as_deref().unwrap()) {
            return Err(ClaudePluginError::Invalid(format!(
                "插件安装路径 canonical 路径越出当前缓存根目录：{id}"
            )));
        }
        for secret in &plugin.sensitive_user_config_keys {
            if plugin.public_user_config.contains_key(secret) {
                return Err(ClaudePluginError::Invalid(format!(
                    "敏感 userConfig 不能存在公开状态：{id}.{secret}"
                )));
            }
        }
    }
    Ok(())
}

/// 强制 ID 含市场命名空间，避免状态、缓存和密钥键冲突。
fn require_marketplace_id(id: &PluginId) -> Result<PluginId> {
    PluginId::from_components(&id.plugin, Some(id.require_marketplace()?)).map_err(Into::into)
}

fn plugin_ids_equal_ascii_case(left: &PluginId, right: &PluginId) -> bool {
    left.marketplace
        .as_deref()
        .zip(right.marketplace.as_deref())
        .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && left.plugin.eq_ignore_ascii_case(&right.plugin)
}

/// 把公开标识转换为安全的单层缓存路径分段。
fn safe_cache_component(value: &str, label: &str) -> Result<String> {
    normalized_identifier(value, label)
}

/// 将插件和市场公开标识转换为不区分大小写的稳定缓存/密钥键。
///
/// 展示用的 [`PluginId`] 仍保留原始大小写；只有持久化命名空间使用 ASCII
/// 小写，避免同一逻辑插件因清单或调用方大小写变化分裂目录和密钥。
fn stable_cache_component(value: &str, label: &str) -> Result<String> {
    Ok(safe_cache_component(value, label)?.to_ascii_lowercase())
}

/// 空 settings 实现，只用于本地清单验证；settings 来源必须由调用方显式解析。
#[cfg(test)]
struct EmptyMarketplaceSettings;

#[cfg(test)]
impl MarketplaceSettings for EmptyMarketplaceSettings {
    /// 验证期间不允许隐式从环境或文件读取 settings。
    fn marketplace_source(&self, _key: &str) -> Option<MarketplaceSource> {
        None
    }
}

#[cfg(test)]
mod tests;
