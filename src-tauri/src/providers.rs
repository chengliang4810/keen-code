use anyhow::{Context, Result};
use keencode_model::{ProviderCapabilities, ProviderProtocol};
use keencode_provider::{
    ApiKey, ProviderConfig as RuntimeProviderConfig, ProviderModelPolicy, ProviderRegistration,
    ProviderRegistry, ProviderRegistrySnapshot,
};
use reqwest::blocking::Client;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tauri::AppHandle;
use url::Url;

use crate::http_response::{HttpResponseReadError, read_http_response_limited};

/// 串行化供应商元数据的读写。
static PROVIDER_IO_LOCK: Mutex<()> = Mutex::new(());

/// 单个 Provider API Key 允许占用的最大字节数。
const MAX_PROVIDER_API_KEY_BYTES: usize = 16 * 1024;
/// 单次供应商模型目录响应允许读取的最大字节数。
const MAX_PROVIDER_MODEL_CATALOG_BYTES: usize = 5 * 1024 * 1024;
/// 本地供应商配置文件允许读取和写入的最大字节数。
const MAX_PROVIDER_CONFIG_BYTES: u64 = 8 * 1024 * 1024;
/// Provider 凭据修订摘要使用的独立哈希域。
const PROVIDER_CREDENTIAL_REVISION_DOMAIN: &[u8] =
    b"keencode-desktop-provider-credential-revision-v1";
/// 当前供应商配置文件的固定 schema 名称。
const PROVIDER_CONFIG_SCHEMA: &str = "keencode/providers";
/// 当前供应商配置文件的固定格式版本。
const PROVIDER_CONFIG_VERSION: u32 = 1;

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
    /// 每模型手工配置的上下文窗口（token）；空 map 表示未配置。
    context_windows: BTreeMap<String, u64>,
    /// 启用 1M 上下文的模型集合；勾选后运行时上下文窗口强制为 1M（最高优先级）。
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
    /// 当前实际交给 Agent Runtime 的模型标识。
    #[serde(deserialize_with = "deserialize_required_option")]
    active_model_id: Option<String>,
    /// 已保存的供应商列表。
    providers: Vec<ProviderRecord>,
}

/// 供应商配置文件的严格版本外壳。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProviderFile {
    /// 固定 schema 名称。
    schema: String,
    /// 固定格式版本。
    version: u32,
    /// 当前完整供应商状态。
    #[serde(flatten)]
    state: ProviderState,
}

impl ProviderFile {
    /// 为当前状态构造完整供应商配置文件。
    fn from_state(state: &ProviderState) -> Self {
        Self {
            schema: PROVIDER_CONFIG_SCHEMA.to_owned(),
            version: PROVIDER_CONFIG_VERSION,
            state: state.clone(),
        }
    }

    /// 校验文件身份并返回当前状态。
    fn into_state(self) -> Result<ProviderState> {
        if self.schema != PROVIDER_CONFIG_SCHEMA || self.version != PROVIDER_CONFIG_VERSION {
            anyhow::bail!("供应商配置 schema 或版本不受支持");
        }
        Ok(self.state)
    }
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

/// 将当前完整配置原子替换到自研 Runtime 的 Provider 注册表。
pub(crate) fn replace_runtime_registry(
    registry: &ProviderRegistry,
    providers: &ProvidersListResult,
) -> Result<ProviderRegistrySnapshot> {
    let registrations = providers
        .providers
        .iter()
        .map(runtime_provider_registration)
        .collect::<Result<Vec<_>>>()?;
    registry
        .replace_all(registrations)
        .context("原子替换 Runtime Provider 注册表失败")
}

/// 把一个持久化 Provider 转换为协议固定、模型集合固定的 Runtime 注册项。
fn runtime_provider_registration(provider: &CustomProvider) -> Result<ProviderRegistration> {
    let config = runtime_provider_config(provider)?;
    ProviderRegistration::new(
        config,
        provider.name.clone(),
        provider_credential_revision(provider.api_key.as_deref()),
        ProviderModelPolicy::Enumerated {
            models: provider.models.clone(),
        },
    )
    .context("构造 Runtime Provider 注册项失败")
}

/// 把桌面配置严格映射为三种 Provider 中立协议之一。
fn runtime_provider_config(provider: &CustomProvider) -> Result<RuntimeProviderConfig> {
    let protocol = match validate_api_backend(&provider.api_backend)? {
        "messages" => ProviderProtocol::Messages,
        "chat_completions" => ProviderProtocol::ChatCompletions,
        "responses" => ProviderProtocol::Responses,
        _ => unreachable!("api_backend 已通过严格校验"),
    };
    let base_url = runtime_provider_base_url(&provider.base_url, protocol)?;
    let mut config = match provider.api_key.as_deref() {
        Some(secret) => RuntimeProviderConfig::new(
            provider.id.clone(),
            protocol,
            base_url,
            ApiKey::new(validate_secret(secret)?.to_owned())?,
        )
        .context("构造带认证的 Runtime Provider 配置失败"),
        None => RuntimeProviderConfig::new_unauthenticated(provider.id.clone(), protocol, base_url)
            .context("构造无认证 Runtime Provider 配置失败"),
    }?;
    config.default_capabilities = ProviderCapabilities {
        streaming: true,
        tool_calling: true,
        ..ProviderCapabilities::default()
    };
    for model in &provider.models {
        let max_context_tokens = if provider.context_1m.get(model).copied().unwrap_or(false) {
            Some(1_000_000)
        } else {
            provider.context_windows.get(model).copied()
        };
        config.model_capabilities.insert(
            model.clone(),
            ProviderCapabilities {
                streaming: true,
                tool_calling: true,
                image_input: provider
                    .supports_vision
                    .get(model)
                    .copied()
                    .unwrap_or(false),
                max_context_tokens,
                ..ProviderCapabilities::default()
            },
        );
    }
    Ok(config)
}

/// 将可选完整端点还原为 Runtime 可安全拼接协议资源的基础地址。
fn runtime_provider_base_url(base_url: &str, protocol: ProviderProtocol) -> Result<String> {
    let api_backend = match protocol {
        ProviderProtocol::Messages => "messages",
        ProviderProtocol::ChatCompletions => "chat_completions",
        ProviderProtocol::Responses => "responses",
    };
    validate_exact_endpoint(base_url, api_backend)?;
    let base_url = validate_base_url(base_url)?;
    let without_marker = base_url
        .strip_suffix('#')
        .unwrap_or(&base_url)
        .trim_end_matches('/');
    let endpoint = match protocol {
        ProviderProtocol::Messages => "/messages",
        ProviderProtocol::ChatCompletions => "/chat/completions",
        ProviderProtocol::Responses => "/responses",
    };
    Ok(without_marker
        .strip_suffix(endpoint)
        .unwrap_or(without_marker)
        .trim_end_matches('/')
        .to_owned())
}

/// 生成随密钥变化且不包含密钥正文的稳定凭据修订值。
fn provider_credential_revision(api_key: Option<&str>) -> String {
    let mut digest = Sha256::new();
    digest.update(PROVIDER_CREDENTIAL_REVISION_DOMAIN);
    match api_key {
        Some(secret) => {
            digest.update([1]);
            digest.update((secret.len() as u64).to_be_bytes());
            digest.update(secret.as_bytes());
        }
        None => digest.update([0]),
    }
    let digest = digest.finalize();
    let mut revision = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut revision, "{byte:02x}");
    }
    revision
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
    let bytes = match read_http_response_limited(response, MAX_PROVIDER_MODEL_CATALOG_BYTES) {
        Ok(bytes) => bytes,
        Err(HttpResponseReadError::TooLarge { max_bytes }) => {
            anyhow::bail!("模型目录响应超过 {max_bytes} 字节限制");
        }
        Err(HttpResponseReadError::Read(error)) => {
            return Err(error).context("解析模型目录 JSON 失败");
        }
    };
    let value: Value = serde_json::from_slice(&bytes).context("解析模型目录 JSON 失败")?;
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
    load_state_from_path(&path)
}

/// 从明确路径严格读取供应商状态；只有文件不存在时才返回当前空状态。
fn load_state_from_path(path: &Path) -> Result<ProviderState> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProviderState::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("读取供应商配置失败：{}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("供应商配置路径不是普通文件：{}", path.display());
    }
    let bytes = read_provider_config_bytes(path)?;
    let file: ProviderFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("供应商配置格式无效：{}", path.display()))?;
    let state = file
        .into_state()
        .with_context(|| format!("供应商配置 schema 无效：{}", path.display()))?;
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
    let path = state_path(app)?;
    save_state_to_path(&path, state)
}

/// 在明确路径原子保存当前供应商状态，并拒绝替换符号链接或非普通文件。
fn save_state_to_path(path: &Path, state: &ProviderState) -> Result<()> {
    validate_state(state)?;
    let file = ProviderFile::from_state(state);
    let bytes = serde_json::to_vec_pretty(&file).context("序列化供应商配置失败")?;
    if bytes.len() as u64 > MAX_PROVIDER_CONFIG_BYTES {
        anyhow::bail!("供应商配置超过 {MAX_PROVIDER_CONFIG_BYTES} 字节");
    }
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        anyhow::bail!("供应商配置目标不是可替换的普通文件：{}", path.display());
    }
    crate::storage::atomic_write_private(path, &bytes)
        .with_context(|| format!("保存供应商配置失败：{}", path.display()))
}

/// 在读取前后都限制供应商配置大小，避免损坏或竞态增长的文件耗尽内存。
fn read_provider_config_bytes(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("读取供应商配置元数据失败：{}", path.display()))?;
    if metadata.len() > MAX_PROVIDER_CONFIG_BYTES {
        anyhow::bail!(
            "供应商配置超过 {MAX_PROVIDER_CONFIG_BYTES} 字节：{}",
            path.display()
        );
    }
    let file =
        fs::File::open(path).with_context(|| format!("打开供应商配置失败：{}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_PROVIDER_CONFIG_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("读取供应商配置失败：{}", path.display()))?;
    if bytes.len() as u64 > MAX_PROVIDER_CONFIG_BYTES {
        anyhow::bail!(
            "供应商配置超过 {MAX_PROVIDER_CONFIG_BYTES} 字节：{}",
            path.display()
        );
    }
    Ok(bytes)
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

// 真实长会话压缩测试独立保存；默认测试只执行其离线边界用例。
#[cfg(test)]
#[path = "providers/live_context_tests.rs"]
mod live_context_tests;

#[cfg(test)]
mod tests {
    use super::{
        ProviderFile, ProviderRecord, ProviderState, model_catalog_endpoint, validate_api_key,
        validate_base_url, validate_catalog_secret_scope, validate_context_1m,
        validate_context_windows, validate_exact_endpoint, validate_secret, validate_state,
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
            "apiBackend": "responses",
            "contextWindows": {},
            "context1m": {},
            "supportsVision": {"test-model": false}
        });

        let record = serde_json::from_value::<ProviderRecord>(value).expect("应接受持久化密钥");
        assert_eq!(record.api_key.as_deref(), Some("persisted-key"));
    }

    /// 当前供应商记录必须显式保存每个能力配置，不得从缺失字段推导默认值。
    #[test]
    fn provider_record_rejects_missing_model_capabilities() {
        let value = serde_json::json!({
            "id": "provider",
            "name": "Provider",
            "baseUrl": "https://api.example.com/v1",
            "models": ["test-model"],
            "apiBackend": "responses",
            "apiKey": null,
            "contextWindows": {},
            "context1m": {}
        });

        assert!(serde_json::from_value::<ProviderRecord>(value).is_err());
    }

    /// 每模型上下文窗口必须能按当前持久化结构无损往返。
    #[test]
    fn provider_context_windows_roundtrip() {
        let value = serde_json::json!({
            "id": "provider",
            "name": "Provider",
            "baseUrl": "https://api.example.com/v1",
            "models": ["test-model", "other-model"],
            "apiBackend": "responses",
            "apiKey": null,
            "contextWindows": { "test-model": 128000 },
            "context1m": {},
            "supportsVision": {"test-model": false}
        });

        let record = serde_json::from_value::<ProviderRecord>(value).expect("应接受上下文窗口");
        assert_eq!(record.context_windows.get("test-model"), Some(&128_000));

        let reencoded = serde_json::to_value(&record).expect("应可序列化");
        assert_eq!(reencoded["contextWindows"]["test-model"], 128_000);
    }

    /// 模型能力配置必须拒绝未登记模型和超出合法范围的上下文窗口。
    #[test]
    fn model_capability_validation_rejects_invalid_entries() {
        let models = ["test-model".to_owned()];
        let mut context_windows = BTreeMap::new();
        context_windows.insert("ghost-model".to_owned(), 128_000);
        assert!(validate_context_windows(context_windows, &models).is_err());

        for invalid_window in [100, 99_000_000] {
            let mut context_windows = BTreeMap::new();
            context_windows.insert("test-model".to_owned(), invalid_window);
            assert!(validate_context_windows(context_windows, &models).is_err());
        }

        let mut context_1m = BTreeMap::new();
        context_1m.insert("ghost-model".to_owned(), true);
        assert!(validate_context_1m(context_1m, &models).is_err());
    }

    #[test]
    fn provider_config_rejects_unknown_fields() {
        let value = serde_json::json!({
            "schema": "keencode/providers",
            "version": 1,
            "activeProviderId": "provider",
            "activeModelId": "test-model",
            "expiredTopLevelField": true,
            "providers": [{
                "id": "provider",
                "name": "Provider",
                "baseUrl": "https://api.example.com/v1",
                "models": ["test-model"],
                "apiBackend": "responses",
                "apiKey": null,
                "contextWindows": {},
                "context1m": {},
                "supportsVision": {"test-model": false},
                "expiredProviderField": "ignored"
            }]
        });
        assert!(serde_json::from_value::<ProviderFile>(value).is_err());
    }

    /// 当前配置缺少 activeModelId 时必须直接拒绝，不能自动补选首个模型。
    #[test]
    fn provider_state_rejects_missing_active_model() {
        let value = serde_json::json!({
            "schema": "keencode/providers",
            "version": 1,
            "activeProviderId": "provider",
            "providers": [{
                "id": "provider",
                "name": "Provider",
                "baseUrl": "https://api.example.com/v1",
                "models": ["test-model"],
                "apiBackend": "responses",
                "apiKey": null,
                "contextWindows": {},
                "context1m": {},
                "supportsVision": {"test-model": false}
            }]
        });

        assert!(serde_json::from_value::<ProviderFile>(value).is_err());
    }

    /// 首次持久化的空状态也必须显式写出两个可空激活字段。
    #[test]
    fn provider_state_accepts_explicit_current_empty_shape() {
        let value = serde_json::json!({
            "schema": "keencode/providers",
            "version": 1,
            "activeProviderId": null,
            "activeModelId": null,
            "providers": []
        });

        let state = serde_json::from_value::<ProviderFile>(value)
            .expect("应接受当前空配置")
            .into_state()
            .expect("schema/version 应有效");
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

#[cfg(test)]
mod provider_registry_tests {
    use super::{
        CustomProvider, MAX_PROVIDER_CONFIG_BYTES, ProviderState, ProvidersListResult,
        load_state_from_path, provider_credential_revision, replace_runtime_registry,
        runtime_provider_config, save_state_to_path,
    };
    use keencode_model::{ModelProvider, ProviderCapabilities, ProviderProtocol};
    use keencode_provider::ProviderRegistry;
    use std::collections::BTreeMap;
    use std::fs;

    /// 构造一个只包含当前结构字段的 Runtime Provider 测试配置。
    fn provider(
        id: &str,
        base_url: &str,
        api_backend: &str,
        api_key: Option<&str>,
        model: &str,
    ) -> CustomProvider {
        CustomProvider {
            id: id.to_owned(),
            models: vec![model.to_owned()],
            base_url: base_url.to_owned(),
            name: id.to_owned(),
            api_backend: api_backend.to_owned(),
            api_key: api_key.map(str::to_owned),
            context_windows: [(model.to_owned(), 64_000)].into_iter().collect(),
            context_1m: BTreeMap::new(),
            supports_vision: [(model.to_owned(), true)].into_iter().collect(),
        }
    }

    /// 三种协议都必须剥离当前资源后缀，避免 ProviderConfig 再次重复拼接。
    #[test]
    fn maps_three_protocols_and_strips_generation_endpoint() {
        for (backend, protocol, endpoint) in [
            ("messages", ProviderProtocol::Messages, "messages"),
            (
                "chat_completions",
                ProviderProtocol::ChatCompletions,
                "chat/completions",
            ),
            ("responses", ProviderProtocol::Responses, "responses"),
        ] {
            let provider = provider(
                backend,
                &format!("https://models.example/v2/{endpoint}#"),
                backend,
                Some("test-key"),
                "test-model",
            );
            let config = runtime_provider_config(&provider).expect("协议配置应映射");
            assert_eq!(config.protocol, protocol);
            assert_eq!(config.base_url().as_str(), "https://models.example/v2/");
            assert!(config.has_authentication());
            let capabilities = config.capabilities_for("test-model");
            assert!(capabilities.streaming);
            assert!(capabilities.tool_calling);
            assert!(capabilities.image_input);
            assert_eq!(capabilities.max_context_tokens, Some(64_000));
        }
    }

    /// 无密钥 Provider 必须保留为明确无认证客户端，不生成空凭据。
    #[test]
    fn maps_unauthenticated_provider_without_fake_secret() {
        let provider = provider(
            "local",
            "http://127.0.0.1:11434/v1/responses",
            "responses",
            None,
            "local-model",
        );
        let config = runtime_provider_config(&provider).expect("本机无认证配置应映射");
        assert!(!config.has_authentication());
        assert_eq!(config.base_url().as_str(), "http://127.0.0.1:11434/v1/");
    }

    /// 注册表能力按 1M、手工窗口、未配置的优先级生成，且始终保留基础流式与工具能力。
    #[test]
    fn registry_maps_context_capability_priority_and_default() {
        let mut provider = provider(
            "gateway",
            "https://models.example/v1/chat/completions",
            "chat_completions",
            Some("test-key"),
            "manual-model",
        );
        provider.models = vec![
            "manual-model".to_owned(),
            "million-model".to_owned(),
            "default-model".to_owned(),
        ];
        provider
            .context_windows
            .insert("manual-model".to_owned(), 128_000);
        provider
            .context_windows
            .insert("million-model".to_owned(), 256_000);
        provider.context_1m.insert("million-model".to_owned(), true);

        let registry = ProviderRegistry::new();
        replace_runtime_registry(
            &registry,
            &ProvidersListResult {
                providers: vec![provider],
                default_model: Some("manual-model".to_owned()),
                active_provider_id: Some("gateway".to_owned()),
            },
        )
        .expect("模型能力应注册");

        let manual = registry
            .resolve("gateway", "manual-model")
            .expect("手工窗口模型应解析")
            .capabilities("manual-model");
        assert!(manual.streaming);
        assert!(manual.tool_calling);
        assert_eq!(manual.max_context_tokens, Some(128_000));

        let million = registry
            .resolve("gateway", "million-model")
            .expect("1M 模型应解析")
            .capabilities("million-model");
        assert_eq!(million.max_context_tokens, Some(1_000_000));

        let default = registry
            .resolve("gateway", "default-model")
            .expect("未配置窗口模型应解析")
            .capabilities("default-model");
        assert_eq!(default.max_context_tokens, None);
    }

    /// 完整替换必须注册全部供应商，并按独立 Provider 与精确模型字段隔离解析。
    #[test]
    fn registry_maps_every_provider_with_exact_model_policy() {
        let registry = ProviderRegistry::new();
        let snapshot = replace_runtime_registry(
            &registry,
            &ProvidersListResult {
                providers: vec![
                    provider(
                        "openai",
                        "https://models.example/v1/chat/completions",
                        "chat_completions",
                        Some("key-a"),
                        "openai-model",
                    ),
                    provider(
                        "anthropic",
                        "https://models.example/v1/messages",
                        "messages",
                        Some("key-b"),
                        "anthropic-model",
                    ),
                ],
                default_model: Some("openai-model".to_owned()),
                active_provider_id: Some("openai".to_owned()),
            },
        )
        .expect("全部供应商应注册");

        assert_eq!(snapshot.providers.len(), 2);
        assert_eq!(
            registry
                .resolve("openai", "openai-model")
                .expect("OpenAI 模型应解析")
                .protocol(),
            ProviderProtocol::ChatCompletions
        );
        assert_eq!(
            registry
                .resolve("anthropic", "anthropic-model")
                .expect("Anthropic 模型应解析")
                .protocol(),
            ProviderProtocol::Messages
        );
        assert!(registry.resolve("openai", "anthropic-model").is_err());
        assert!(registry.resolve("anthropic", "openai-model").is_err());
    }

    /// 任一桌面配置无效时必须拒绝整批替换，并保持上一代注册表完整可用。
    #[test]
    fn invalid_provider_rejects_atomic_replacement() {
        let registry = ProviderRegistry::new();
        let previous = replace_runtime_registry(
            &registry,
            &ProvidersListResult {
                providers: vec![provider(
                    "stable",
                    "https://models.example/v1/responses",
                    "responses",
                    Some("stable-key"),
                    "stable-model",
                )],
                default_model: Some("stable-model".to_owned()),
                active_provider_id: Some("stable".to_owned()),
            },
        )
        .expect("初始供应商应注册");
        let invalid = CustomProvider {
            base_url: "not-a-url".to_owned(),
            ..provider(
                "invalid",
                "https://models.example/v1/responses",
                "responses",
                Some("invalid-key"),
                "invalid-model",
            )
        };

        assert!(
            replace_runtime_registry(
                &registry,
                &ProvidersListResult {
                    providers: vec![
                        provider(
                            "replacement",
                            "https://models.example/v1/responses",
                            "responses",
                            Some("replacement-key"),
                            "replacement-model",
                        ),
                        invalid,
                    ],
                    default_model: Some("replacement-model".to_owned()),
                    active_provider_id: Some("replacement".to_owned()),
                },
            )
            .is_err()
        );
        assert_eq!(registry.snapshot().expect("注册表应可读"), previous);
        assert!(registry.resolve("stable", "stable-model").is_ok());
        assert!(
            registry
                .resolve("replacement", "replacement-model")
                .is_err()
        );
    }

    /// 原子替换后旧解析必须失效，新模型解析使用新的注册表代次。
    #[test]
    fn replacement_invalidates_old_resolution_and_activates_new_snapshot() {
        let registry = ProviderRegistry::new();
        let old_snapshot = replace_runtime_registry(
            &registry,
            &ProvidersListResult {
                providers: vec![provider(
                    "gateway",
                    "https://models.example/v1/responses",
                    "responses",
                    Some("old-test-key"),
                    "old-model",
                )],
                default_model: Some("old-model".to_owned()),
                active_provider_id: Some("gateway".to_owned()),
            },
        )
        .expect("旧配置应注册");
        let old_resolution = registry
            .resolve("gateway", "old-model")
            .expect("旧模型应解析");
        assert!(old_resolution.capabilities("old-model").streaming);

        let new_snapshot = replace_runtime_registry(
            &registry,
            &ProvidersListResult {
                providers: vec![provider(
                    "gateway",
                    "https://models.example/v2/chat/completions",
                    "chat_completions",
                    Some("new-test-key"),
                    "new-model",
                )],
                default_model: Some("new-model".to_owned()),
                active_provider_id: Some("gateway".to_owned()),
            },
        )
        .expect("新配置应原子替换");

        assert!(new_snapshot.generation > old_snapshot.generation);
        assert_eq!(
            old_resolution.capabilities("old-model"),
            ProviderCapabilities::default()
        );
        assert!(registry.resolve("gateway", "old-model").is_err());
        let new_resolution = registry
            .resolve("gateway", "new-model")
            .expect("新模型应解析");
        assert_eq!(new_resolution.protocol(), ProviderProtocol::ChatCompletions);
        assert!(new_resolution.capabilities("new-model").streaming);
        assert_ne!(
            old_snapshot.providers[0].config_identity,
            new_snapshot.providers[0].config_identity
        );
    }

    /// 凭据修订必须稳定、区分空认证与不同密钥且不回显密钥正文。
    #[test]
    fn credential_revision_is_stable_and_redacted() {
        let first = provider_credential_revision(Some("private-test-key"));
        assert_eq!(
            first,
            provider_credential_revision(Some("private-test-key"))
        );
        assert_ne!(
            first,
            provider_credential_revision(Some("another-test-key"))
        );
        assert_ne!(first, provider_credential_revision(None));
        assert!(!first.contains("private-test-key"));
    }

    /// 缺失配置只返回当前空状态，首次保存必须写入严格外壳并可无损读取。
    #[test]
    fn missing_provider_config_returns_empty_and_current_schema_roundtrips() {
        let directory = tempfile::tempdir().expect("创建供应商配置临时目录");
        let path = directory.path().join("providers.json");

        let state = load_state_from_path(&path).expect("缺失配置应返回空状态");
        assert!(state.providers.is_empty());
        assert!(state.active_provider_id.is_none());
        assert!(state.active_model_id.is_none());
        assert!(!path.exists());

        save_state_to_path(&path, &state).expect("当前空配置应可保存");
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted["schema"], "keencode/providers");
        assert_eq!(persisted["version"], 1);
        assert_eq!(persisted["providers"], serde_json::json!([]));
        assert!(load_state_from_path(&path).unwrap().providers.is_empty());
    }

    /// 损坏、未知字段和非当前版本必须失败关闭，且不得覆盖原配置字节。
    #[test]
    fn invalid_provider_config_is_rejected_without_replacement() {
        let directory = tempfile::tempdir().expect("创建供应商配置临时目录");
        let path = directory.path().join("providers.json");
        let cases = [
            b"not-json".as_slice(),
            br#"{"schema":"keencode/providers","version":0,"activeProviderId":null,"activeModelId":null,"providers":[]}"#,
            br#"{"schema":"keencode/providers","version":1,"activeProviderId":null,"activeModelId":null,"providers":[],"unexpected":true}"#,
        ];

        for (index, original) in cases.into_iter().enumerate() {
            fs::write(&path, original).expect("写入非法供应商配置");
            assert!(
                load_state_from_path(&path).is_err(),
                "非法供应商配置 {index} 不应被接受"
            );
            assert_eq!(fs::read(&path).unwrap(), original);
        }
    }

    /// 超限配置与目录目标必须在解析或替换前失败，并保持原目标不变。
    #[test]
    fn oversized_and_non_file_provider_configs_are_rejected() {
        let directory = tempfile::tempdir().expect("创建供应商配置临时目录");
        let oversized = directory.path().join("oversized.json");
        let original = vec![b'x'; MAX_PROVIDER_CONFIG_BYTES as usize + 1];
        fs::write(&oversized, &original).expect("写入超限供应商配置");
        assert!(load_state_from_path(&oversized).is_err());
        assert_eq!(fs::read(&oversized).unwrap(), original);

        let non_file = directory.path().join("directory.json");
        fs::create_dir(&non_file).expect("创建供应商配置目录目标");
        assert!(load_state_from_path(&non_file).is_err());
        assert!(save_state_to_path(&non_file, &ProviderState::default()).is_err());
        assert!(non_file.is_dir());
    }
}
