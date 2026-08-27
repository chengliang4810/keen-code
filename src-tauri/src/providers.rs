use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs::OpenOptions, process};
use tauri::AppHandle;
use url::Url;

/// 串行化供应商元数据的读写。
static PROVIDER_IO_LOCK: Mutex<()> = Mutex::new(());

/// 单个 Provider API Key 允许占用的最大字节数。
const MAX_PROVIDER_API_KEY_BYTES: usize = 16 * 1024;

/// KeenCode 持久化的自定义供应商记录。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProviderRecord {
    /// 供应商稳定标识。
    id: String,
    /// 界面展示名称。
    name: String,
    /// 模型 API 基础地址。
    base_url: String,
    /// 供应商下允许在任务中选择的模型标识。
    models: Vec<String>,
    /// 请求协议类型。
    api_backend: String,
    /// 已保存的 API Key；None 表示该供应商无认证。
    api_key: Option<String>,
    /// 每模型手工配置的上下文窗口（token）；缺省表示未配置（自动获取或回退默认）。
    #[serde(default)]
    context_windows: BTreeMap<String, u64>,
    /// 启用 1M 上下文的模型集合；勾选后运行时上下文窗口强制为 1M（最高优先级）。
    #[serde(default)]
    context_1m: BTreeMap<String, bool>,
    /// 每模型是否支持图片输入；未勾选的模型保存为 false。
    supports_vision: BTreeMap<String, bool>,
}

/// KeenCode 自有的供应商配置文件结构。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProviderState {
    /// 当前激活的供应商标识。
    #[serde(deserialize_with = "deserialize_required_option")]
    active_provider_id: Option<String>,
    /// 当前实际交给 peri 的模型标识。
    #[serde(deserialize_with = "deserialize_required_option")]
    active_model_id: Option<String>,
    /// 已保存的供应商列表。
    providers: Vec<ProviderRecord>,
}

/// 反序列化必须显式存在、但允许写为 null 的当前字段。
fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// 返回给前端的自定义供应商。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomProvider {
    /// 供应商稳定标识。
    pub id: String,
    /// 供应商下可用的模型标识。
    pub models: Vec<String>,
    /// 模型 API 基础地址。
    pub base_url: String,
    /// 界面展示名称。
    pub name: String,
    /// 请求协议类型。
    pub api_backend: String,
    /// 已保存的 API Key，供前端显示/隐藏查看；None 表示无认证。
    pub api_key: Option<String>,
    /// 每模型手工配置的上下文窗口（token）；空 map 表示全部未配置。
    pub context_windows: BTreeMap<String, u64>,
    /// 启用 1M 上下文的模型集合；空 map 表示全部未启用。
    pub context_1m: BTreeMap<String, bool>,
    /// 每模型是否支持图片输入。
    pub supports_vision: BTreeMap<String, bool>,
}

/// 模型设置页所需的完整供应商状态。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvidersListResult {
    /// 已保存的供应商列表。
    pub providers: Vec<CustomProvider>,
    /// 当前激活供应商的默认模型。
    pub default_model: Option<String>,
    /// 当前激活供应商标识。
    pub active_provider_id: Option<String>,
}

/// 供应商表单的新增或更新参数。
#[derive(Clone, Debug)]
pub struct ProviderUpsert {
    /// 供应商稳定标识。
    pub id: String,
    /// 供应商下允许使用的模型标识。
    pub models: Vec<String>,
    /// 模型 API 基础地址。
    pub base_url: String,
    /// 可选展示名称。
    pub name: Option<String>,
    /// 请求协议类型。
    pub api_backend: String,
    /// 可选 API Key；Some 覆盖保存，None 清空该供应商密钥。
    pub api_key: Option<String>,
    /// 每模型手工配置的上下文窗口（token）；空 map 表示全部未配置。
    pub context_windows: BTreeMap<String, u64>,
    /// 启用 1M 上下文的模型集合；空 map 表示全部未启用。
    pub context_1m: BTreeMap<String, bool>,
    /// 每模型是否支持图片输入。
    pub supports_vision: BTreeMap<String, bool>,
    /// 是否只允许创建新记录。
    pub create_only: bool,
}

/// 远端模型目录中的单个模型。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    /// 模型标识。
    pub id: String,
    /// 远端返回的所有者或展示名称。
    pub owned_by: Option<String>,
    /// 远端返回的上下文窗口（token）；目录接口未提供时为 None。
    pub context_window: Option<u64>,
}

/// 模型目录查询结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelsResult {
    /// 远端返回的模型列表。
    pub models: Vec<ProviderModel>,
}

/// 返回当前供应商配置列表。
pub fn list(app: &AppHandle) -> Result<ProvidersListResult> {
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let state = load_state(app)?;
    Ok(render_list(state))
}

/// 新增或更新一个自定义供应商。
pub fn upsert(app: &AppHandle, input: ProviderUpsert) -> Result<ProvidersListResult> {
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let id = validate_provider_id(&input.id)?;
    let base_url = validate_base_url(&input.base_url)?;
    let models = normalize_models(input.models)?;
    let api_backend = validate_api_backend(&input.api_backend)?;
    validate_exact_endpoint(&base_url, api_backend)?;
    // 所见即所得：Some 覆盖保存密钥，None 清空该供应商认证。
    let api_key = validate_api_key(input.api_key.as_deref())?;
    let mut state = load_state(app)?;
    let existing_index = state
        .providers
        .iter()
        .position(|provider| provider.id == id);
    if input.create_only && existing_index.is_some() {
        anyhow::bail!("供应商 {id} 已存在");
    }

    let name = input
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&id)
        .to_string();
    let context_windows = validate_context_windows(input.context_windows, &models)?;
    let context_1m = validate_context_1m(input.context_1m, &models)?;
    let supports_vision = validate_supports_vision(input.supports_vision, &models)?;
    let record = ProviderRecord {
        id: id.clone(),
        name,
        base_url,
        models: models.clone(),
        api_backend: api_backend.to_string(),
        api_key,
        context_windows,
        context_1m,
        supports_vision,
    };
    if let Some(index) = existing_index {
        state.providers[index] = record;
    } else {
        state.providers.push(record);
    }
    if state.active_provider_id.is_none() {
        state.active_provider_id = Some(id.clone());
        state.active_model_id = models.first().cloned();
    } else if state.active_provider_id.as_deref() == Some(id.as_str())
        && state
            .active_model_id
            .as_ref()
            .is_none_or(|model| !models.contains(model))
    {
        state.active_model_id = models.first().cloned();
    }
    save_state(app, &state)?;
    Ok(render_list(state))
}

/// 删除一个自定义供应商。
pub fn remove(app: &AppHandle, provider_id: &str) -> Result<ProvidersListResult> {
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let id = validate_provider_id(provider_id)?;
    let mut state = load_state(app)?;
    let original_len = state.providers.len();
    state.providers.retain(|provider| provider.id != id);
    if state.providers.len() == original_len {
        anyhow::bail!("找不到供应商 {id}");
    }
    if state.active_provider_id.as_deref() == Some(id.as_str()) {
        state.active_provider_id = state.providers.first().map(|provider| provider.id.clone());
        state.active_model_id = state
            .providers
            .first()
            .and_then(|provider| provider.models.first().cloned());
    }
    save_state(app, &state)?;
    Ok(render_list(state))
}

/// 选择指定供应商下的模型并同步运行时配置。
pub fn select_model(
    app: &AppHandle,
    provider_id: &str,
    model_id: &str,
) -> Result<ProvidersListResult> {
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let mut state = load_state(app)?;
    let provider_id = provider_id.trim();
    let model_id = model_id.trim();
    if provider_id.is_empty() {
        anyhow::bail!("供应商标识不能为空");
    }
    if model_id.is_empty() {
        anyhow::bail!("模型标识不能为空");
    }
    let provider = state
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .with_context(|| format!("找不到供应商 {provider_id}"))?;
    if !provider.models.iter().any(|model| model == model_id) {
        anyhow::bail!("供应商 {provider_id} 中找不到模型 {model_id}");
    }
    state.active_provider_id = Some(provider.id.clone());
    state.active_model_id = Some(model_id.to_string());
    save_state(app, &state)?;
    Ok(render_list(state))
}

/// 从标准模型目录接口读取可选模型。
pub fn list_models(
    base_url: &str,
    api_key: Option<&str>,
    api_backend: &str,
) -> Result<ProviderModelsResult> {
    let base_url = validate_base_url(base_url)?;
    let backend = validate_api_backend(api_backend)?;
    let endpoint = model_catalog_endpoint(&base_url, backend);
    let secret = validate_api_key(api_key)?;

    request_models(&endpoint, backend, secret.as_deref())
}

/// 校验模型目录请求与已登记供应商完全一致，避免复用密钥到其他地址。
pub fn validate_model_catalog_scope(
    app: &AppHandle,
    provider_id: &str,
    base_url: &str,
    api_backend: &str,
) -> Result<()> {
    let base_url = validate_base_url(base_url)?;
    let api_backend = validate_api_backend(api_backend)?;
    let id = validate_provider_id(provider_id)?;
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let state = load_state(app)?;
    let provider = state
        .providers
        .iter()
        .find(|provider| provider.id == id)
        .with_context(|| format!("找不到供应商 {id}"))?;
    validate_catalog_secret_scope(provider, &base_url, api_backend)
}

/// 请求已经校验完成的模型目录地址并解析模型列表。
fn request_models(
    endpoint: &str,
    backend: &str,
    api_key: Option<&str>,
) -> Result<ProviderModelsResult> {
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .context("创建模型目录 HTTP 客户端失败")?;
    let mut request = client.get(endpoint);
    if let Some(secret) = api_key {
        if backend == "messages" {
            request = request
                .header("x-api-key", secret)
                .header("anthropic-version", "2023-06-01");
        } else {
            request = request.bearer_auth(secret);
        }
    }
    let response = request
        .send()
        .with_context(|| format!("请求模型目录失败：{endpoint}"))?
        .error_for_status()
        .with_context(|| format!("模型目录返回错误：{endpoint}"))?;
    let value: Value = response.json().context("解析模型目录 JSON 失败")?;
    let rows = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .cloned()
        .unwrap_or_default();
    let mut models = rows
        .into_iter()
        .filter_map(|item| {
            let id = item
                .get("id")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)?
                .trim()
                .to_string();
            if id.is_empty() {
                return None;
            }
            let owned_by = item
                .get("owned_by")
                .or_else(|| item.get("display_name"))
                .and_then(Value::as_str)
                .map(str::to_string);
            // 兼容端点常以不同字段名返回上下文窗口；非数字忽略，尽力而为。
            let context_window = ["context_window", "context_length", "max_context_length"]
                .iter()
                .find_map(|key| item.get(*key).and_then(Value::as_u64))
                .or_else(|| item.get("max_input_tokens").and_then(Value::as_u64));
            Some(ProviderModel {
                id,
                owned_by,
                context_window,
            })
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(ProviderModelsResult { models })
}

/// 将持久化状态投影成前端所需结构。
fn render_list(state: ProviderState) -> ProvidersListResult {
    let active_provider_id = state.active_provider_id.clone();
    let default_model = state.active_model_id.clone();
    let providers = state
        .providers
        .into_iter()
        .map(|provider| CustomProvider {
            id: provider.id,
            models: provider.models,
            base_url: provider.base_url,
            name: provider.name,
            api_backend: provider.api_backend,
            api_key: provider.api_key,
            context_windows: provider.context_windows,
            context_1m: provider.context_1m,
            supports_vision: provider.supports_vision,
        })
        .collect();
    ProvidersListResult {
        providers,
        default_model,
        active_provider_id,
    }
}

/// 读取供应商状态文件。
fn load_state(app: &AppHandle) -> Result<ProviderState> {
    let path = state_path(app)?;
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProviderState::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("读取供应商配置失败：{}", path.display()));
        }
    };
    let state: ProviderState = serde_json::from_str(&content)
        .with_context(|| format!("供应商配置格式无效：{}", path.display()))?;
    validate_state(&state)?;
    Ok(state)
}

/// 校验磁盘中的供应商配置必须完整符合当前唯一结构，不做自动修正。
fn validate_state(state: &ProviderState) -> Result<()> {
    let mut provider_ids = std::collections::HashSet::new();
    for provider in &state.providers {
        let id = validate_provider_id(&provider.id)?;
        if id != provider.id {
            anyhow::bail!("供应商标识必须使用规范格式：{}", provider.id);
        }
        if !provider_ids.insert(provider.id.as_str()) {
            anyhow::bail!("供应商标识重复：{}", provider.id);
        }
        if provider.name.trim().is_empty() || provider.name.trim() != provider.name {
            anyhow::bail!("供应商 {} 的名称不能为空或包含首尾空白", provider.id);
        }
        if validate_base_url(&provider.base_url)? != provider.base_url {
            anyhow::bail!("供应商 {} 的 API 地址不是规范格式", provider.id);
        }
        if let Err(error) = validate_exact_endpoint(&provider.base_url, &provider.api_backend) {
            anyhow::bail!("供应商 {} 的 API 地址不合规：{error}", provider.id);
        }
        if normalize_models(provider.models.clone())? != provider.models {
            anyhow::bail!(
                "供应商 {} 的模型列表包含空项、重复项或首尾空白",
                provider.id
            );
        }
        validate_context_windows(provider.context_windows.clone(), &provider.models)?;
        validate_context_1m(provider.context_1m.clone(), &provider.models)?;
        validate_supports_vision(provider.supports_vision.clone(), &provider.models)?;
        if validate_api_backend(&provider.api_backend)? != provider.api_backend {
            anyhow::bail!("供应商 {} 的协议类型不是规范格式", provider.id);
        }
        if provider.api_key.is_some() {
            validate_api_key(provider.api_key.as_deref())?;
        }
    }

    match (
        state.providers.is_empty(),
        state.active_provider_id.as_deref(),
        state.active_model_id.as_deref(),
    ) {
        (true, None, None) => Ok(()),
        (true, _, _) => anyhow::bail!("没有供应商时不能保存当前供应商或模型"),
        (false, Some(provider_id), Some(model_id)) => {
            let provider = state
                .providers
                .iter()
                .find(|provider| provider.id == provider_id)
                .with_context(|| format!("当前供应商不存在：{provider_id}"))?;
            if !provider.models.iter().any(|model| model == model_id) {
                anyhow::bail!("当前模型 {model_id} 不属于供应商 {provider_id}");
            }
            Ok(())
        }
        (false, _, _) => anyhow::bail!("存在供应商时必须同时保存当前供应商和当前模型"),
    }
}

/// 手工配置上下文窗口的下限（1K tokens）。
const MIN_CONTEXT_WINDOW: u64 = 1_024;
/// 手工配置上下文窗口的上限（10M tokens）。
const MAX_CONTEXT_WINDOW: u64 = 10_000_000;

/// 校验每模型上下文窗口配置：key 必须属于模型列表，值必须在合法区间。
fn validate_context_windows(
    context_windows: BTreeMap<String, u64>,
    models: &[String],
) -> Result<BTreeMap<String, u64>> {
    for (model, window) in &context_windows {
        if !models.iter().any(|item| item == model) {
            anyhow::bail!("上下文窗口配置的模型 {model} 不在供应商模型列表中");
        }
        if !(MIN_CONTEXT_WINDOW..=MAX_CONTEXT_WINDOW).contains(window) {
            anyhow::bail!(
                "模型 {model} 的上下文窗口 {window} 超出合法范围（{MIN_CONTEXT_WINDOW}..{MAX_CONTEXT_WINDOW}）"
            );
        }
    }
    Ok(context_windows)
}

/// 校验 1M 上下文模型集合：key 必须属于模型列表（值仅 true/false 无需范围校验）。
fn validate_context_1m(
    context_1m: BTreeMap<String, bool>,
    models: &[String],
) -> Result<BTreeMap<String, bool>> {
    for model in context_1m.keys() {
        if !models.iter().any(|item| item == model) {
            anyhow::bail!("1M 上下文配置的模型 {model} 不在供应商模型列表中");
        }
    }
    Ok(context_1m)
}

/// 校验视觉能力配置：每个模型都必须显式保存 true 或 false。
fn validate_supports_vision(
    supports_vision: BTreeMap<String, bool>,
    models: &[String],
) -> Result<BTreeMap<String, bool>> {
    for model in models {
        if !supports_vision.contains_key(model) {
            anyhow::bail!("模型 {model} 缺少视觉能力配置");
        }
    }
    for model in supports_vision.keys() {
        if !models.iter().any(|item| item == model) {
            anyhow::bail!("视觉能力配置的模型 {model} 不在供应商模型列表中");
        }
    }
    Ok(supports_vision)
}

/// 校验、去重并稳定保留模型列表顺序。
fn normalize_models(models: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for model in models {
        let model = model.trim();
        if model.is_empty() || normalized.iter().any(|item| item == model) {
            continue;
        }
        if model.chars().any(char::is_control) {
            anyhow::bail!("模型标识不能包含控制字符");
        }
        normalized.push(model.to_string());
    }
    if normalized.is_empty() {
        anyhow::bail!("至少需要添加一个模型");
    }
    Ok(normalized)
}

/// 原子写入供应商状态文件。
fn save_state(app: &AppHandle, state: &ProviderState) -> Result<()> {
    validate_state(state)?;
    let path = state_path(app)?;
    let bytes = serde_json::to_vec_pretty(state).context("序列化供应商配置失败")?;
    write_private_file(&path, &bytes)?;
    Ok(())
}

/// 校验 API Key，不自动裁剪或修复输入。
fn validate_secret(secret: &str) -> Result<&str> {
    if secret.is_empty() {
        anyhow::bail!("API Key 不能为空");
    }
    if secret.trim() != secret {
        anyhow::bail!("API Key 不能包含首尾空白");
    }
    if secret.chars().any(char::is_control) {
        anyhow::bail!("API Key 不能包含控制字符");
    }
    if secret.len() > MAX_PROVIDER_API_KEY_BYTES {
        anyhow::bail!("API Key 超过大小限制");
    }
    Ok(secret)
}

/// 校验可选 API Key；None 明确表示无认证。
pub fn validate_api_key(api_key: Option<&str>) -> Result<Option<String>> {
    api_key
        .map(validate_secret)
        .transpose()
        .map(|secret| secret.map(str::to_owned))
}

/// 限制已保存密钥只能发送到其登记时的地址和协议。
fn validate_catalog_secret_scope(
    provider: &ProviderRecord,
    base_url: &str,
    api_backend: &str,
) -> Result<()> {
    if provider.base_url != base_url || provider.api_backend != api_backend {
        anyhow::bail!("供应商地址或协议已变更，请输入对应 API Key 后再拉取模型");
    }
    Ok(())
}

/// 以仅当前用户可读写的权限写文件。
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("私有文件路径缺少父目录")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("创建私有文件目录失败：{}", parent.display()))?;
    let temporary = private_temporary_path(path)?;
    let write_result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("创建私有临时文件失败：{}", temporary.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("写入私有临时文件失败：{}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("同步私有临时文件失败：{}", temporary.display()))?;
        drop(file);
        fs::rename(&temporary, path)
            .with_context(|| format!("替换私有文件失败：{}", path.display()))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("设置私有文件权限失败：{}", path.display()))?;
    }
    Ok(())
}

/// 生成同目录私有临时文件路径，保证后续 rename 不跨文件系统。
fn private_temporary_path(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().context("私有文件路径缺少父目录")?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("私有文件路径缺少文件名")?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(parent.join(format!(".{file_name}.{}.{}.tmp", process::id(), nanos)))
}

/// 返回供应商状态文件路径。
fn state_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::storage::root_dir(app)?.join("providers.json"))
}

/// 校验供应商稳定标识。
fn validate_provider_id(raw: &str) -> Result<String> {
    let id = raw.trim();
    if id.is_empty() || id.len() > 64 {
        anyhow::bail!("供应商标识长度必须为 1 到 64 个字符");
    }
    let mut characters = id.chars();
    if !characters
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        anyhow::bail!("供应商标识只能使用字母、数字、点、下划线和短横线");
    }
    Ok(id.to_string())
}

/// 校验并标准化模型 API 基础地址。
///
/// 末尾单独一个 `#` 是"完整路径"标记：声明该地址即最终请求端点，运行时不再
/// 追加 `/v1` 或协议端点后缀。标记原样保留在持久化值中，仅在映射运行时配置时剥离。
fn validate_base_url(raw: &str) -> Result<String> {
    let value = raw.trim().trim_end_matches('/');
    let mut parsed = Url::parse(value).context("模型 API 地址无效")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        anyhow::bail!("模型 API 地址必须是有效的 http 或 https 地址");
    }
    match parsed.fragment() {
        // 命名片段（如 `#section`）不是完整路径标记，且会污染下游地址拼接。
        Some(fragment) if !fragment.is_empty() => {
            anyhow::bail!("模型 API 地址不支持 # 片段，# 仅可作为末尾的完整路径标记");
        }
        // 完整路径标记要求用户显式给出请求路径，不做 `/v1` 自动补全。
        Some(_) if parsed.path().is_empty() || parsed.path() == "/" => {
            anyhow::bail!("以 # 结尾的地址必须包含完整的请求路径");
        }
        // 用户只填写服务域名时自动使用标准 `/v1` 路径；显式填写的自定义路径保持不变。
        None if parsed.path().is_empty() || parsed.path() == "/" => parsed.set_path("/v1"),
        _ => {}
    }
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

/// 校验带 `#` 完整路径标记的地址与所选协议匹配。
///
/// 运行时仅在请求路径以协议端点（`/chat/completions`、`/responses`、`/messages`）
/// 结尾时才原样使用，其他形态会被追加路径导致请求错误地址，必须在此提前拒绝。
fn validate_exact_endpoint(base_url: &str, api_backend: &str) -> Result<()> {
    if !base_url.ends_with('#') {
        return Ok(());
    }
    let suffix = match validate_api_backend(api_backend)? {
        "responses" => "/responses",
        "chat_completions" => "/chat/completions",
        _ => "/messages",
    };
    if !base_url
        .trim_end_matches('#')
        .trim_end_matches('/')
        .ends_with(suffix)
    {
        anyhow::bail!("以 # 结尾的完整路径地址必须以 {suffix} 结尾");
    }
    Ok(())
}

/// 校验请求协议类型。
fn validate_api_backend(raw: &str) -> Result<&str> {
    match raw.trim() {
        "responses" => Ok("responses"),
        "chat_completions" => Ok("chat_completions"),
        "messages" => Ok("messages"),
        _ => anyhow::bail!("不支持的模型协议：{raw}"),
    }
}

/// 从 API 基础地址生成标准模型目录地址。
fn model_catalog_endpoint(base_url: &str, api_backend: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    // `#` 完整路径：剥掉标记与协议端点后缀，在与生成端点同级的目录查询模型列表。
    if trimmed.ends_with('#') {
        let suffix = match api_backend {
            "responses" => "/responses",
            "chat_completions" => "/chat/completions",
            _ => "/messages",
        };
        let stripped = trimmed.trim_end_matches('#').trim_end_matches('/');
        let base = stripped.strip_suffix(suffix).unwrap_or(stripped);
        return format!("{base}/models");
    }
    let base = if api_backend == "messages" {
        trimmed
            .strip_suffix("/messages")
            .unwrap_or(trimmed)
            .trim_end_matches("/v1")
    } else {
        trimmed
            .strip_suffix("/responses")
            .or_else(|| trimmed.strip_suffix("/chat/completions"))
            .unwrap_or(trimmed)
    };
    if api_backend == "messages" {
        return format!("{base}/v1/models");
    }
    format!("{base}/models")
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderRecord, ProviderState, model_catalog_endpoint, validate_api_key, validate_base_url,
        validate_catalog_secret_scope, validate_exact_endpoint, validate_secret, validate_state,
    };
    use std::collections::BTreeMap;

    /// 只填写域名时自动补全标准 `/v1`，自定义 API 路径不应被覆盖。
    #[test]
    fn provider_base_url_makes_v1_path_optional() {
        assert_eq!(
            validate_base_url("https://api.example.com").unwrap(),
            "https://api.example.com/v1"
        );
        assert_eq!(
            validate_base_url("https://api.example.com/custom").unwrap(),
            "https://api.example.com/custom"
        );
    }

    /// Anthropic Messages 根地址必须查询官方 `/v1/models`。
    #[test]
    fn anthropic_catalog_uses_v1_models() {
        assert_eq!(
            model_catalog_endpoint("https://api.anthropic.com", "messages"),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(
            model_catalog_endpoint("https://api.anthropic.com/v1/messages", "messages"),
            "https://api.anthropic.com/v1/models"
        );
    }

    /// OpenAI 类协议保持标准 `/models` 目录规则。
    #[test]
    fn openai_catalog_strips_known_generation_endpoint() {
        assert_eq!(
            model_catalog_endpoint("https://api.example/v1/responses", "responses"),
            "https://api.example/v1/models"
        );
        assert_eq!(
            model_catalog_endpoint(
                "https://api.example/v1/chat/completions",
                "chat_completions"
            ),
            "https://api.example/v1/models"
        );
    }

    /// 末尾 `#` 是完整路径标记：原样保留持久化，且不做 `/v1` 自动补全。
    #[test]
    fn provider_base_url_preserves_exact_path_marker() {
        assert_eq!(
            validate_base_url("https://api.example.com/v2/chat/completions#").unwrap(),
            "https://api.example.com/v2/chat/completions#"
        );
        assert!(validate_base_url("https://api.example.com#").is_err());
        assert!(validate_base_url("https://api.example.com/v1#section").is_err());
    }

    /// `#` 完整路径的模型目录在与生成端点同级的 `/models` 上查询。
    #[test]
    fn exact_path_catalog_uses_sibling_models() {
        assert_eq!(
            model_catalog_endpoint(
                "https://api.example/v2/chat/completions#",
                "chat_completions"
            ),
            "https://api.example/v2/models"
        );
        assert_eq!(
            model_catalog_endpoint("https://api.example/v2/responses#", "responses"),
            "https://api.example/v2/models"
        );
        assert_eq!(
            model_catalog_endpoint("https://api.example/v2/messages#", "messages"),
            "https://api.example/v2/models"
        );
    }

    /// `#` 完整路径必须以所选协议的生成端点结尾，否则运行时会拼出错误地址。
    #[test]
    fn exact_path_marker_requires_protocol_endpoint_suffix() {
        assert!(
            validate_exact_endpoint(
                "https://api.example/v2/chat/completions#",
                "chat_completions"
            )
            .is_ok()
        );
        assert!(
            validate_exact_endpoint(
                "https://api.example/v2/chat/completions",
                "chat_completions"
            )
            .is_ok()
        );
        assert!(validate_exact_endpoint("https://api.example/v2#", "chat_completions").is_err());
        assert!(
            validate_exact_endpoint("https://api.example/v2/messages#", "chat_completions")
                .is_err()
        );
    }

    /// 供应商元数据接受 API Key 字段，密钥持久化到磁盘配置。
    #[test]
    fn provider_record_persists_api_key() {
        let value = serde_json::json!({
            "id": "provider",
            "name": "Provider",
            "baseUrl": "https://api.example.com/v1",
            "models": ["test-model"],
            "apiKey": "persisted-key",
            "apiBackend": "responses"
        });

        let record = serde_json::from_value::<ProviderRecord>(value).expect("应接受持久化密钥");
        assert_eq!(record.api_key.as_deref(), Some("persisted-key"));
    }

    /// 当前配置缺少 activeModelId 时必须直接拒绝，不能自动补选首个模型。
    #[test]
    fn provider_state_rejects_missing_active_model() {
        let value = serde_json::json!({
            "activeProviderId": "provider",
            "providers": [{
                "id": "provider",
                "name": "Provider",
                "baseUrl": "https://api.example.com/v1",
                "models": ["test-model"],
                "apiBackend": "responses"
            }]
        });

        assert!(serde_json::from_value::<ProviderState>(value).is_err());
    }

    /// 首次持久化的空状态也必须显式写出两个可空激活字段。
    #[test]
    fn provider_state_accepts_explicit_current_empty_shape() {
        let value = serde_json::json!({
            "activeProviderId": null,
            "activeModelId": null,
            "providers": []
        });

        let state = serde_json::from_value::<ProviderState>(value).expect("应接受当前空配置");
        assert!(validate_state(&state).is_ok());
    }

    /// 当前配置中的激活项必须精确指向同一供应商下的现有模型。
    #[test]
    fn provider_state_rejects_inconsistent_selection() {
        let state = ProviderState {
            active_provider_id: Some("provider".to_string()),
            active_model_id: Some("missing-model".to_string()),
            providers: vec![ProviderRecord {
                id: "provider".to_string(),
                name: "Provider".to_string(),
                base_url: "https://api.example.com/v1".to_string(),
                models: vec!["test-model".to_string()],
                api_backend: "responses".to_string(),
                api_key: None,
                context_windows: BTreeMap::new(),
                context_1m: BTreeMap::new(),
                supports_vision: [("test-model".to_string(), false)].into_iter().collect(),
            }],
        };

        assert!(validate_state(&state).is_err());
    }

    /// 密钥不得通过裁剪来接受非规范输入。
    #[test]
    fn provider_secret_rejects_empty_or_padded_values() {
        assert_eq!(validate_secret("secret-key").unwrap(), "secret-key");
        assert!(validate_secret("").is_err());
        assert!(validate_secret(" secret-key").is_err());
        assert!(validate_secret("secret-key\n").is_err());
        assert!(validate_secret("secret\u{7f}key").is_err());
    }

    /// 可选密钥中的 None 必须明确保留为无认证。
    #[test]
    fn absent_provider_secret_means_no_authentication() {
        assert_eq!(validate_api_key(None).unwrap(), None);
        assert_eq!(
            validate_api_key(Some("secret-key")).unwrap(),
            Some("secret-key".to_string())
        );
    }

    /// 已保存密钥不得被模型目录请求发送到其他地址或协议。
    #[test]
    fn provider_secret_is_scoped_to_saved_endpoint() {
        let provider = ProviderRecord {
            id: "provider".to_string(),
            name: "Provider".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            models: vec!["test-model".to_string()],
            api_backend: "responses".to_string(),
            api_key: None,
            context_windows: BTreeMap::new(),
            context_1m: BTreeMap::new(),
            supports_vision: [("test-model".to_string(), true)].into_iter().collect(),
        };

        assert!(
            validate_catalog_secret_scope(&provider, "https://api.example.com/v1", "responses")
                .is_ok()
        );
        assert!(
            validate_catalog_secret_scope(
                &provider,
                "https://attacker.example.com/v1",
                "responses"
            )
            .is_err()
        );
        assert!(
            validate_catalog_secret_scope(&provider, "https://api.example.com/v1", "messages")
                .is_err()
        );
    }
}

// ── (KeenCode) peri 供应商映射 ────────────────────────────────────────────────────

/// 去掉用户可能填写的完整协议端点后缀。
fn strip_endpoint_suffix(base_url: &str, suffix: &str) -> String {
    base_url
        .trim()
        .trim_end_matches('/')
        .strip_suffix(suffix)
        .unwrap_or_else(|| base_url.trim().trim_end_matches('/'))
        .to_string()
}

/// 将全部已保存供应商映射进 `PeriConfig.config.providers`（而非仅当前激活的一个）。
///
/// 会话级 provider 隔离（Q1 决策）下，任意会话可以通过 `session/set_config_option`
/// 的 `"{provider_id}::{model}"` 值切换到任意已保存供应商，`LlmProvider::from_provider_config`
/// 按 `provider_id` 在 `cfg.peri_config.config.providers` 中查找——因此该列表必须
/// 包含全部供应商。
///
/// 不产出隐式模型字段；调用方（`peri_runtime.rs`）负责另行决定
/// "新会话默认 provider"。无法映射的供应商
/// （地址非法等）会被跳过并记录警告日志，不影响其余供应商。
pub fn build_peri_config_all(providers: Vec<CustomProvider>) -> peri_acp::provider::PeriConfig {
    use peri_acp::provider::{AppConfig, PeriConfig};

    let mapped = providers
        .into_iter()
        .filter_map(|provider| match map_provider_config(&provider) {
            Ok(cfg) => Some(cfg),
            Err(error) => {
                tracing::warn!(provider_id = %provider.id, %error, "跳过无法映射的供应商配置");
                None
            }
        })
        .collect();

    PeriConfig {
        schema: None,
        config: AppConfig {
            providers: mapped,
            ..AppConfig::default()
        },
    }
}

/// 将单个 [`CustomProvider`] 映射为 peri 的 `ProviderConfig`。
///
/// 协议推断与 base_url 端点后缀剥离逻辑与桌面端原单供应商映射保持一致。
fn map_provider_config(provider: &CustomProvider) -> Result<peri_acp::provider::ProviderConfig> {
    use peri_acp::provider::ProviderConfig;

    let id = validate_provider_id(&provider.id)?;
    let base_url = validate_base_url(&provider.base_url)?;
    let api_backend = validate_api_backend(&provider.api_backend)?;
    let api_key = provider
        .api_key
        .as_deref()
        .map(validate_secret)
        .transpose()?
        .map(str::to_owned);

    let provider_type = match api_backend {
        "responses" => "openai_responses",
        "chat_completions" => "openai",
        "messages" => "anthropic",
        _ => unreachable!("api_backend 已通过严格校验"),
    }
    .to_string();
    let base_url = if let Some(exact) = base_url.strip_suffix('#') {
        // `#` 完整路径：仅去掉标记，把用户填写的端点原样交给运行时。
        exact.trim_end_matches('/').to_string()
    } else {
        match api_backend {
            "responses" => strip_endpoint_suffix(&base_url, "/responses"),
            "chat_completions" => strip_endpoint_suffix(&base_url, "/chat/completions"),
            _ => base_url,
        }
    };

    let mut extra = serde_json::Map::new();
    extra.insert(
        "supportsVision".to_string(),
        serde_json::to_value(&provider.supports_vision).context("序列化模型视觉能力配置失败")?,
    );
    Ok(ProviderConfig {
        id,
        provider_type,
        api_key: api_key.unwrap_or_default(),
        base_url,
        name: Some(provider.name.clone()),
        models: peri_acp::provider::ProviderModels::default(),
        extra,
    })
}

/// 解析某供应商某模型的运行时上下文参数：1M 标志（最高优先级）→ 手工配置 → 默认。
///
/// 返回 `(context_1m, context_window)`；1M 开启时手工配置被忽略（与旧单供应商
/// 映射语义一致，由适配器按 1M 处理）。供 `peri_runtime.rs` 构造会话默认 provider。
pub(crate) fn resolve_context(provider: &CustomProvider, model: &str) -> (bool, Option<u32>) {
    let context_1m = provider.context_1m.get(model).copied().unwrap_or(false);
    let context_window = if context_1m {
        None
    } else {
        provider
            .context_windows
            .get(model)
            .copied()
            .and_then(|value| u32::try_from(value).ok())
    };
    (context_1m, context_window)
}

#[cfg(test)]
mod peri_mapping_tests {
    use super::{
        CustomProvider, ProviderRecord, build_peri_config_all, map_provider_config,
        resolve_context, validate_context_1m, validate_context_windows,
    };
    use peri_acp::provider::{AgentModelResolution, LlmProvider};
    use std::collections::BTreeMap;

    fn provider(
        id: &str,
        base_url: &str,
        api_backend: &str,
        api_key: Option<&str>,
    ) -> CustomProvider {
        CustomProvider {
            id: id.to_string(),
            models: vec!["test-model".to_string()],
            base_url: base_url.to_string(),
            name: id.to_string(),
            api_backend: api_backend.to_string(),
            api_key: api_key.map(str::to_string),
            context_windows: BTreeMap::new(),
            context_1m: BTreeMap::new(),
            supports_vision: [("test-model".to_string(), true)].into_iter().collect(),
        }
    }

    /// Responses 配置必须映射为 openai_responses，并能构造真实 Responses Provider。
    #[test]
    fn builds_openai_responses_provider() {
        let mapped = map_provider_config(&provider(
            "openai",
            "https://models.example/v1/responses",
            "responses",
            Some("test-key"),
        ))
        .expect("Responses 配置应可映射");
        assert_eq!(mapped.provider_type, "openai_responses");
        assert_eq!(mapped.base_url, "https://models.example/v1");

        let config = build_peri_config_all(vec![provider(
            "openai",
            "https://models.example/v1/responses",
            "responses",
            Some("test-key"),
        )]);
        let llm = LlmProvider::from_provider_config(
            &config,
            "openai",
            "test-model",
            Some("high".to_string()),
            32000,
            false,
            None,
        )
        .expect("Responses 配置应可构造 LlmProvider");

        assert!(matches!(
            &llm,
            peri_acp::provider::LlmProvider::OpenAiResponses { .. }
        ));
        assert!(llm.supports_vision());
    }

    /// `#` 完整路径标记：映射时仅剥离标记，非 `/v1` 版本前缀的完整端点原样交给运行时。
    #[test]
    fn exact_path_marker_maps_full_endpoint_verbatim() {
        let mapped = map_provider_config(&provider(
            "tencent",
            "https://copilot.tencent.com/v2/chat/completions#",
            "chat_completions",
            Some("test-key"),
        ))
        .expect("完整路径配置应可映射");
        assert_eq!(mapped.provider_type, "openai");
        assert_eq!(
            mapped.base_url,
            "https://copilot.tencent.com/v2/chat/completions"
        );
    }

    /// 无密钥的供应商按上游语义视为未配置，不构造伪造的 LlmProvider。
    #[test]
    fn provider_without_api_key_is_unconfigured() {
        let config = build_peri_config_all(vec![provider(
            "no-auth",
            "https://models.example/v1/chat/completions",
            "chat_completions",
            None,
        )]);
        assert_eq!(config.config.providers[0].api_key, "");
        assert!(
            LlmProvider::from_provider_config(
                &config,
                "no-auth",
                "example-model",
                Some("high".to_string()),
                32000,
                false,
                None,
            )
            .is_none()
        );
    }

    /// 空密钥/带首尾空白的密钥必须返回错误，禁止绕过校验。
    #[test]
    fn rejects_empty_runtime_key_without_panicking() {
        assert!(
            map_provider_config(&provider(
                "invalid",
                "https://models.example/v1",
                "responses",
                Some("")
            ))
            .is_err()
        );
        assert!(
            map_provider_config(&provider(
                "invalid",
                "https://models.example/v1",
                "responses",
                Some(" test-key")
            ))
            .is_err()
        );
    }

    /// 旧版配置文件没有 contextWindows 字段时按未配置解析（向后兼容）。
    #[test]
    fn provider_record_accepts_missing_context_windows() {
        let value = serde_json::json!({
            "id": "provider",
            "name": "Provider",
            "baseUrl": "https://api.example.com/v1",
            "models": ["test-model"],
            "apiBackend": "responses"
        });
        let record = serde_json::from_value::<ProviderRecord>(value).expect("应接受旧版配置");
        assert!(record.context_windows.is_empty());
    }

    /// contextWindows 必须能持久化并往返。
    #[test]
    fn context_windows_roundtrip() {
        let value = serde_json::json!({
            "id": "provider",
            "name": "Provider",
            "baseUrl": "https://api.example.com/v1",
            "models": ["test-model", "other-model"],
            "apiBackend": "responses",
            "contextWindows": { "test-model": 128000 }
        });
        let record =
            serde_json::from_value::<ProviderRecord>(value).expect("应接受 contextWindows");
        assert_eq!(record.context_windows.get("test-model"), Some(&128_000));

        let reencoded = serde_json::to_value(&record).expect("应可序列化");
        assert_eq!(reencoded["contextWindows"]["test-model"], 128_000);
    }

    /// 校验拒绝未登记模型的上下文窗口配置。
    #[test]
    fn context_windows_reject_unknown_model() {
        let mut map = BTreeMap::new();
        map.insert("ghost-model".to_string(), 128_000);
        let result = validate_context_windows(map, &["test-model".to_string()]);
        assert!(result.is_err());
    }

    /// 校验拒绝超出合法范围的上下文窗口值。
    #[test]
    fn context_windows_reject_out_of_range() {
        let mut map = BTreeMap::new();
        map.insert("test-model".to_string(), 100);
        assert!(validate_context_windows(map, &["test-model".to_string()]).is_err());

        let mut map = BTreeMap::new();
        map.insert("test-model".to_string(), 99_000_000);
        assert!(validate_context_windows(map, &["test-model".to_string()]).is_err());
    }

    /// 手工配置的上下文窗口必须解析出来，并透传到 LlmProvider。
    #[test]
    fn build_peri_config_passes_context_window() {
        let mut p = provider(
            "openai",
            "https://models.example/v1/chat/completions",
            "chat_completions",
            Some("test-key"),
        );
        p.context_windows.insert("test-model".to_string(), 128_000);
        let (context_1m, context_window) = resolve_context(&p, "test-model");
        assert!(!context_1m);
        assert_eq!(context_window, Some(128_000));

        let config = build_peri_config_all(vec![p]);
        let llm = LlmProvider::from_provider_config(
            &config,
            "openai",
            "test-model",
            Some("high".to_string()),
            32000,
            context_1m,
            context_window,
        )
        .expect("配置应可构造");
        assert_eq!(llm.context_window(), 128_000);
    }

    /// 1M 标志开启时强制忽略手工配置的上下文窗口。
    #[test]
    fn build_peri_config_1m_flag_overrides_context_window() {
        let mut p = provider(
            "openai",
            "https://models.example/v1/chat/completions",
            "chat_completions",
            Some("test-key"),
        );
        p.context_1m.insert("test-model".to_string(), true);
        p.context_windows.insert("test-model".to_string(), 128_000);
        let (context_1m, context_window) = resolve_context(&p, "test-model");
        assert!(context_1m);
        assert_eq!(context_window, None);

        let config = build_peri_config_all(vec![p]);
        let llm = LlmProvider::from_provider_config(
            &config,
            "openai",
            "test-model",
            Some("high".to_string()),
            32000,
            context_1m,
            context_window,
        )
        .expect("配置应可构造");
        assert!(llm.context_1m());
        assert_eq!(llm.context_window(), 200_000);
    }

    /// 1M 标志校验拒绝未登记模型的配置。
    #[test]
    fn context_1m_reject_unknown_model() {
        let mut map = BTreeMap::new();
        map.insert("ghost-model".to_string(), true);
        assert!(validate_context_1m(map, &["test-model".to_string()]).is_err());
    }

    /// 未配置时上下文参数保持默认，运行时上下文窗口由 peri 回退默认值。
    #[test]
    fn build_peri_config_defaults_context_window() {
        let p = provider(
            "openai",
            "https://models.example/v1/chat/completions",
            "chat_completions",
            Some("test-key"),
        );
        let (context_1m, context_window) = resolve_context(&p, "test-model");
        assert!(!context_1m);
        assert_eq!(context_window, None);

        let config = build_peri_config_all(vec![p]);
        let llm = LlmProvider::from_provider_config(
            &config,
            "openai",
            "test-model",
            Some("high".to_string()),
            32000,
            context_1m,
            context_window,
        )
        .expect("配置应可构造");
        assert_eq!(llm.context_window(), 200_000);
    }

    /// 全部已保存供应商（而非仅激活的一个）必须出现在 peri 配置列表中，且各自可按 id 解析。
    #[test]
    fn build_peri_config_all_maps_every_provider() {
        let config = build_peri_config_all(vec![
            provider(
                "openai",
                "https://models.example/v1/chat/completions",
                "chat_completions",
                Some("key-a"),
            ),
            provider(
                "anthropic",
                "https://models.example/v1",
                "messages",
                Some("key-b"),
            ),
        ]);
        assert_eq!(config.config.providers.len(), 2);
        assert!(
            LlmProvider::from_provider_config(
                &config,
                "openai",
                "test-model",
                Some("high".to_string()),
                32000,
                false,
                None,
            )
            .is_some()
        );
        assert!(
            LlmProvider::from_provider_config(
                &config,
                "anthropic",
                "test-model",
                Some("high".to_string()),
                32000,
                false,
                None,
            )
            .is_some()
        );
    }

    /// KeenCode 的真实运行时配置只接受 provider_id::model；省略模型由宿主继承会话。
    #[test]
    fn build_peri_config_all_accepts_qualified_agent_model_only() {
        let config = build_peri_config_all(vec![provider(
            "openai",
            "https://models.example/v1/chat/completions",
            "chat_completions",
            Some("key-a"),
        )]);
        let inherited = LlmProvider::from_provider_config(
            &config,
            "openai",
            "session-model",
            Some("high".to_string()),
            32000,
            false,
            None,
        )
        .expect("会话 Provider 应可构造");

        assert!(matches!(
            LlmProvider::resolve_agent_model(&config, &inherited, "openai::session-model"),
            AgentModelResolution::Resolved(_)
        ));
        for selection in ["", "unqualified-model"] {
            assert!(matches!(
                LlmProvider::resolve_agent_model(&config, &inherited, selection),
                AgentModelResolution::Error(_)
            ));
        }
    }

    /// 无法映射的供应商（非法地址等）被跳过，不影响其余供应商。
    #[test]
    fn build_peri_config_all_skips_invalid_provider() {
        let bad = CustomProvider {
            base_url: "not-a-url".to_string(),
            ..provider(
                "bad",
                "https://api.example.com/v1",
                "responses",
                Some("key"),
            )
        };
        let config = build_peri_config_all(vec![
            bad,
            provider(
                "openai",
                "https://models.example/v1",
                "responses",
                Some("key"),
            ),
        ]);
        assert_eq!(config.config.providers.len(), 1);
        assert_eq!(config.config.providers[0].id, "openai");
    }
}
