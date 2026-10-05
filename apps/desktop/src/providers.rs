use anyhow::{Context, Result};
use keencode_model::{ProviderCapabilities, ProviderProtocol};
use keencode_provider::{
    ApiKey, ChatOutputTokenField, ProviderConfig as RuntimeProviderConfig, ProviderModelPolicy,
    ProviderRegistration, ProviderRegistry, ProviderRegistrySnapshot,
};
use reqwest::blocking::Client;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
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
/// Facade 新建 Provider 的暂存地址；仅用于通过当前文件 schema，绝不进入 Runtime。
const FACADE_PLACEHOLDER_BASE_URL: &str = "https://localhost.invalid/v1/responses";
/// 原生真实请求的临时超时入口；只在隔离 native-desktop-tests 中读取。
#[cfg(any(test, feature = "native-desktop-tests"))]
const NATIVE_PROVIDER_TIMEOUT_ENV: &str = "KEENCODE_NATIVE_PROVIDER_TIMEOUT_MS";
#[cfg(any(test, feature = "native-desktop-tests"))]
const NATIVE_PROVIDER_TIMEOUT_MIN_MS: u64 = 1;
#[cfg(any(test, feature = "native-desktop-tests"))]
const NATIVE_PROVIDER_TIMEOUT_MAX_MS: u64 = 300_000;
/// 模型未填写上下文窗口时采用的保守默认值。
pub(crate) const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 200_000;
/// 供应商导出文档的固定 schema 名称；导入同时接受完整配置文件 schema。
const PROVIDER_EXPORT_SCHEMA: &str = "keencode/providers-export";

/// 解析原生验收的请求级硬超时；仅用于真实请求超时注入，不模拟模型响应。
///
/// 参数显式传入而不是在测试中改写进程环境，避免并发测试互相污染。
#[cfg(any(test, feature = "native-desktop-tests"))]
fn parse_native_provider_timeout_override(
    native_desktop_tests: bool,
    benchmark_enabled: bool,
    benchmark_data_dir_present: bool,
    raw_timeout_ms: Option<&str>,
) -> Result<Option<Duration>> {
    if !(native_desktop_tests && benchmark_enabled && benchmark_data_dir_present) {
        return Ok(None);
    }
    let Some(raw_timeout_ms) = raw_timeout_ms else {
        return Ok(None);
    };
    let timeout_ms = raw_timeout_ms.parse::<u64>().with_context(|| {
        format!(
            "{NATIVE_PROVIDER_TIMEOUT_ENV} 必须是 {NATIVE_PROVIDER_TIMEOUT_MIN_MS}..={NATIVE_PROVIDER_TIMEOUT_MAX_MS} 的整数"
        )
    })?;
    if !(NATIVE_PROVIDER_TIMEOUT_MIN_MS..=NATIVE_PROVIDER_TIMEOUT_MAX_MS).contains(&timeout_ms) {
        anyhow::bail!(
            "{NATIVE_PROVIDER_TIMEOUT_ENV} 必须是 {NATIVE_PROVIDER_TIMEOUT_MIN_MS}..={NATIVE_PROVIDER_TIMEOUT_MAX_MS} 的整数"
        );
    }
    Ok(Some(Duration::from_millis(timeout_ms)))
}

/// 生产构建完全不读取临时超时变量；原生验收必须同时处于三个隔离条件。
fn native_provider_timeout_override_from_environment() -> Result<Option<Duration>> {
    #[cfg(not(feature = "native-desktop-tests"))]
    {
        Ok(None)
    }

    #[cfg(feature = "native-desktop-tests")]
    {
        let benchmark_enabled = std::env::var("KEENCODE_BENCHMARK").as_deref() == Ok("1");
        let benchmark_data_dir_present = std::env::var_os("KEENCODE_BENCHMARK_DATA_DIR").is_some();
        if !(benchmark_enabled && benchmark_data_dir_present) {
            return Ok(None);
        }
        let raw_timeout_ms = std::env::var_os(NATIVE_PROVIDER_TIMEOUT_ENV)
            .map(|value| {
                value.to_str().map(str::to_owned).ok_or_else(|| {
                    anyhow::anyhow!("{NATIVE_PROVIDER_TIMEOUT_ENV} 必须是 UTF-8 的十进制整数")
                })
            })
            .transpose()?;
        parse_native_provider_timeout_override(
            true,
            benchmark_enabled,
            benchmark_data_dir_present,
            raw_timeout_ms.as_deref(),
        )
    }
}

/// KeenCode 持久化的自定义供应商记录。
///
/// 不设 `deny_unknown_fields`：磁盘配置按版本演进时会出现已移除字段，加载必须
/// 忽略它们而不是整体失败。被忽略的字段由 {@link unknown_field_warnings} 记录。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderRecord {
    /// 供应商稳定标识。
    id: String,
    /// 界面展示名称。
    name: String,
    /// 可选的原始 Provider Template 身份；个人 Provider 无模板时保持为空。
    #[serde(default)]
    template_id: Option<String>,
    /// 模型 API 基础地址。
    base_url: String,
    /// 供应商下允许在任务中选择的模型标识。
    models: Vec<String>,
    /// 请求协议类型。
    api_backend: String,
    /// 已保存的 API Key；None 表示该供应商无认证。
    api_key: Option<String>,
    /// 每模型手工配置的上下文窗口（token）；缺项在运行时回退 200K。
    context_windows: BTreeMap<String, u64>,
    /// 每模型输出预算；未配置时采用 128000。
    #[serde(default)]
    max_output_tokens: BTreeMap<String, u32>,
    /// 未指定时采用标准 Chat 参数；兼容网关可以显式选择 max_tokens。
    #[serde(default)]
    chat_output_token_field: ChatOutputTokenField,
    /// 每模型是否支持图片输入；未勾选的模型保存为 false。
    supports_vision: BTreeMap<String, bool>,
    /// 每模型显式开放的推理档位；缺项沿用公共模型目录。
    #[serde(default)]
    reasoning_efforts: BTreeMap<String, Vec<String>>,
    /// 仍保留在设置视图中、但不得进入 Runtime 注册表的模型。
    #[serde(default)]
    disabled_models: BTreeSet<String>,
    /// 模型级稀疏配置覆盖，按 source facade 的 ModelConfigObject 保存。
    #[serde(default)]
    model_configs: BTreeMap<String, Value>,
}

/// KeenCode 自有的供应商配置文件结构。
///
/// 不设 `deny_unknown_fields`：配置按版本演进后磁盘上会出现已移除字段，
/// 启动加载必须忽略它们继续运行。被忽略的字段由加载期诊断记录。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderState {
    /// 当前激活的供应商标识。
    #[serde(deserialize_with = "deserialize_required_option")]
    active_provider_id: Option<String>,
    /// 当前实际交给 Agent Runtime 的模型标识。
    #[serde(deserialize_with = "deserialize_required_option")]
    active_model_id: Option<String>,
    /// 对外 Settings/Selection facade 使用的持久递增版本。
    ///
    /// 旧配置没有该字段时从零开始；每次成功写入一项变更前递增，避免用
    /// 哈希摘要充当版本导致前端比较失效或超过 JavaScript 安全整数范围。
    #[serde(default)]
    revision: u64,
    /// 已保存的供应商列表。
    providers: Vec<ProviderRecord>,
}

/// 供应商配置文件的版本外壳；schema 与 version 必须显式匹配当前值。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
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
    pub max_output_tokens: BTreeMap<String, u32>,
    pub chat_output_token_field: ChatOutputTokenField,
    /// 每模型是否支持图片输入。
    pub supports_vision: BTreeMap<String, bool>,
    /// 每模型显式开放的推理档位；缺项沿用公共模型目录。
    pub reasoning_efforts: BTreeMap<String, Vec<String>>,
    /// 设置视图中的禁用模型；该字段只供桌面 RPC 与 Runtime 边界使用，
    /// 旧的 providers 列表 JSON 不暴露它，权威值仍写入 providers.json。
    #[serde(skip)]
    pub disabled_models: BTreeSet<String>,
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
    pub max_output_tokens: BTreeMap<String, u32>,
    pub chat_output_token_field: ChatOutputTokenField,
    /// 每模型是否支持图片输入。
    pub supports_vision: BTreeMap<String, bool>,
    /// 每模型显式开放的推理档位；缺项沿用公共模型目录。
    pub reasoning_efforts: BTreeMap<String, Vec<String>>,
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

/// 原页面只编辑已存在供应商的模型目录，不接收或重新写入凭据表单。
/// expected 用于拒绝旧目录覆盖并发更新；模型和供应商都必须使用准确身份。
pub fn update_models(
    app: &AppHandle,
    expected: Vec<String>,
    models: Vec<String>,
) -> Result<ProvidersListResult> {
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    update_models_at_path(&state_path(app)?, expected, models)
}

/// 持锁调用：先完整校验，再一次原子提交所有供应商的模型变化。
fn update_models_at_path(
    path: &Path,
    expected: Vec<String>,
    models: Vec<String>,
) -> Result<ProvidersListResult> {
    let mut state = load_state_from_path(path)?;
    let current: Vec<String> = state
        .providers
        .iter()
        .flat_map(|provider| {
            provider
                .models
                .iter()
                .map(|model| format!("{}::{model}", provider.id))
        })
        .collect();
    if expected != current {
        anyhow::bail!("模型目录已变化，请刷新设置后重试");
    }
    if models.len() > 4096 || models.iter().any(|model| model.len() > 256) {
        anyhow::bail!("模型目录超过大小限制");
    }
    let mut grouped: BTreeMap<String, Vec<String>> = state
        .providers
        .iter()
        .map(|provider| (provider.id.clone(), Vec::new()))
        .collect();
    for qualified in models {
        let (provider_id, model_id) = qualified
            .split_once("::")
            .context("模型必须使用 providerId::modelId 格式")?;
        if model_id.is_empty()
            || model_id.trim() != model_id
            || model_id.chars().any(char::is_control)
        {
            anyhow::bail!("模型标识不能为空、包含控制字符或首尾空白");
        }
        let entries = grouped
            .get_mut(provider_id)
            .context("模型所属供应商尚未配置")?;
        if entries.iter().any(|entry| entry == model_id) {
            anyhow::bail!("模型目录不能包含重复项");
        }
        entries.push(model_id.to_owned());
    }
    for provider in &mut state.providers {
        let entries = grouped.remove(&provider.id).expect("已建立全部供应商分组");
        if entries.is_empty() {
            anyhow::bail!("每个已配置供应商至少保留一个模型");
        }
        provider.models = entries;
        // 保留仍存在模型的手工参数；删除模型不能留下失效的参数键。
        provider
            .context_windows
            .retain(|model, _| provider.models.contains(model));
        provider
            .max_output_tokens
            .retain(|model, _| provider.models.contains(model));
        provider
            .supports_vision
            .retain(|model, _| provider.models.contains(model));
        provider
            .reasoning_efforts
            .retain(|model, _| provider.models.contains(model));
        provider
            .disabled_models
            .retain(|model| provider.models.contains(model));
        provider
            .model_configs
            .retain(|model, _| provider.models.contains(model));
        for model in &provider.models {
            provider
                .supports_vision
                .entry(model.clone())
                .or_insert(false);
        }
        if state.active_provider_id.as_deref() == Some(provider.id.as_str())
            && state
                .active_model_id
                .as_ref()
                .is_none_or(|model| !provider.models.contains(model))
        {
            state.active_model_id = provider.models.first().cloned();
        }
    }
    repair_selection(&mut state);
    bump_revision(&mut state);
    save_state_to_path(path, &state)?;
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
        .filter(|provider| {
            !is_facade_placeholder_url(&provider.base_url)
                && provider
                    .models
                    .iter()
                    .any(|model| !provider.disabled_models.contains(model))
        })
        .map(runtime_provider_registration)
        .collect::<Result<Vec<_>>>()?;
    registry
        .replace_all(registrations)
        .context("原子替换 Runtime Provider 注册表失败")
}

/// 把一个持久化 Provider 转换为协议固定、模型集合固定的 Runtime 注册项。
fn runtime_provider_registration(provider: &CustomProvider) -> Result<ProviderRegistration> {
    let config = runtime_provider_config(provider)?;
    let models = provider
        .models
        .iter()
        .filter(|model| !provider.disabled_models.contains(*model))
        .cloned()
        .collect::<Vec<_>>();
    if models.is_empty() {
        anyhow::bail!("供应商 {} 没有可执行模型", provider.id);
    }
    ProviderRegistration::new(
        config,
        provider.name.clone(),
        provider_credential_revision(provider.api_key.as_deref()),
        ProviderModelPolicy::Enumerated { models },
    )
    .context("构造 Runtime Provider 注册项失败")
}

/// 把桌面配置严格映射为三种 Provider 中立协议之一。
pub(crate) fn runtime_provider_config(provider: &CustomProvider) -> Result<RuntimeProviderConfig> {
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
    if let Some(timeout) = native_provider_timeout_override_from_environment()? {
        config.request_timeout = Some(timeout);
    }
    config.chat_output_token_field = provider.chat_output_token_field;
    // Anthropic Messages 是 cache_control 提示缓存语义唯一有效的协议：该协议
    // 后端自动启用 prompt_caching（与 per-model 能力快照同源下发），其他后端
    // 维持关闭；per-provider 开关 UI 留待后续。
    let prompt_caching = matches!(protocol, ProviderProtocol::Messages);
    config.default_capabilities = ProviderCapabilities {
        streaming: true,
        tool_calling: true,
        prompt_caching,
        ..ProviderCapabilities::default()
    };
    for model in &provider.models {
        if provider.disabled_models.contains(model) {
            continue;
        }
        let max_context_tokens = provider
            .context_windows
            .get(model)
            .copied()
            .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS);
        config.model_capabilities.insert(
            model.clone(),
            ProviderCapabilities {
                streaming: true,
                tool_calling: true,
                prompt_caching,
                image_input: provider
                    .supports_vision
                    .get(model)
                    .copied()
                    .unwrap_or(false),
                max_output_tokens: Some(u64::from(
                    provider
                        .max_output_tokens
                        .get(model)
                        .copied()
                        .unwrap_or(128_000),
                )),
                max_context_tokens: Some(max_context_tokens),
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
    let previous_disabled_models = existing_index
        .and_then(|index| state.providers.get(index))
        .map(|provider| {
            provider
                .disabled_models
                .iter()
                .filter(|model| models.contains(*model))
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let previous_model_configs = existing_index
        .and_then(|index| state.providers.get(index))
        .map(|provider| {
            provider
                .model_configs
                .iter()
                .filter(|(model, _)| models.contains(*model))
                .map(|(model, config)| (model.clone(), config.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
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
    let template_id = existing_index
        .and_then(|index| state.providers.get(index))
        .and_then(|provider| provider.template_id.clone());
    let context_windows = validate_context_windows(input.context_windows, &models)?;
    let max_output_tokens = validate_max_output_tokens(input.max_output_tokens, &models)?;
    let supports_vision = validate_supports_vision(input.supports_vision, &models)?;
    let reasoning_efforts = validate_reasoning_efforts(input.reasoning_efforts, &models)?;
    let record = ProviderRecord {
        id: id.clone(),
        name,
        template_id,
        base_url,
        models: models.clone(),
        api_backend: api_backend.to_string(),
        api_key,
        context_windows,
        max_output_tokens,
        chat_output_token_field: input.chat_output_token_field,
        supports_vision,
        reasoning_efforts,
        disabled_models: previous_disabled_models,
        model_configs: previous_model_configs,
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
    repair_selection(&mut state);
    bump_revision(&mut state);
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
    repair_selection(&mut state);
    bump_revision(&mut state);
    save_state(app, &state)?;
    Ok(render_list(state))
}

/// 供应商导出文档结构；记录结构与持久化配置完全一致，含明文 API Key。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProviderExportFile {
    /// 固定 schema 名称。
    schema: String,
    /// 固定格式版本。
    version: u32,
    /// 导出的供应商记录。
    providers: Vec<ProviderRecord>,
}

/// 供应商导入结果：合并后的完整状态与本次计数。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvidersImportResult {
    /// 合并后的供应商列表。
    pub providers: Vec<CustomProvider>,
    /// 当前激活供应商的默认模型。
    pub default_model: Option<String>,
    /// 当前激活供应商标识。
    pub active_provider_id: Option<String>,
    /// 本次新增的供应商数量。
    pub added: usize,
    /// 本次按同标识覆盖的供应商数量。
    pub updated: usize,
}

/// 导出单个供应商配置 JSON 文档。
pub fn export(app: &AppHandle, provider_id: &str) -> Result<String> {
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let state = load_state(app)?;
    let id = validate_provider_id(provider_id)?;
    let record = state
        .providers
        .iter()
        .find(|provider| provider.id == id)
        .with_context(|| format!("找不到供应商 {id}"))?;
    let file = ProviderExportFile {
        schema: PROVIDER_EXPORT_SCHEMA.to_owned(),
        version: PROVIDER_CONFIG_VERSION,
        providers: vec![record.clone()],
    };
    let bytes = serde_json::to_vec_pretty(&file).context("序列化供应商导出失败")?;
    String::from_utf8(bytes).context("供应商导出内容不是有效 UTF-8")
}

/// 解析导入文本：接受导出文档与完整配置文件两种 schema，返回严格校验前的记录。
fn parse_provider_import(config: &str) -> Result<Vec<ProviderRecord>> {
    if config.len() as u64 > MAX_PROVIDER_CONFIG_BYTES {
        anyhow::bail!("供应商导入内容超过 {MAX_PROVIDER_CONFIG_BYTES} 字节");
    }
    let value: Value = serde_json::from_str(config).context("供应商导入内容不是有效 JSON")?;
    let schema = value
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if schema != PROVIDER_EXPORT_SCHEMA && schema != PROVIDER_CONFIG_SCHEMA {
        anyhow::bail!("供应商导入 schema 不受支持：{schema}");
    }
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    if version != u64::from(PROVIDER_CONFIG_VERSION) {
        anyhow::bail!("供应商导入版本不受支持：{version}");
    }
    let records: Vec<ProviderRecord> = serde_json::from_value(
        value
            .get("providers")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("供应商导入内容缺少 providers 列表"))?,
    )
    .context("供应商导入记录无效")?;
    if records.is_empty() {
        anyhow::bail!("供应商导入内容至少需要包含一个供应商");
    }
    let mut seen = std::collections::HashSet::new();
    for record in &records {
        if !seen.insert(record.id.as_str()) {
            anyhow::bail!("供应商导入内容包含重复标识：{}", record.id);
        }
    }
    Ok(records)
}

/// 将导入记录合并进当前状态：同标识覆盖，其余追加；不主动切换当前选中。
fn merge_provider_import(
    mut state: ProviderState,
    records: Vec<ProviderRecord>,
) -> Result<(ProviderState, usize, usize)> {
    let mut added = 0usize;
    let mut updated = 0usize;
    for record in records {
        match state
            .providers
            .iter()
            .position(|provider| provider.id == record.id)
        {
            Some(index) => {
                updated += 1;
                let keeps_current_provider =
                    state.active_provider_id.as_deref() == Some(record.id.as_str());
                state.providers[index] = record;
                // 与表单保存同源：当前激活模型被导入记录移除时回退到该供应商首个模型。
                if keeps_current_provider
                    && state.active_model_id.as_ref().is_none_or(|model| {
                        !state.providers[index]
                            .models
                            .iter()
                            .any(|item| item == model)
                    })
                {
                    state.active_model_id = state.providers[index].models.first().cloned();
                }
            }
            None => {
                added += 1;
                let is_first_provider = state.providers.is_empty();
                state.providers.push(record);
                // 空状态首次导入与表单保存同源：补齐当前供应商与模型。
                if is_first_provider {
                    let provider = state.providers.last().expect("刚追加的供应商记录");
                    state.active_provider_id = Some(provider.id.clone());
                    state.active_model_id = provider.models.first().cloned();
                }
            }
        }
    }
    repair_selection(&mut state);
    validate_state(&state)?;
    Ok((state, added, updated))
}

/// 导入供应商配置并按标识合并保存；除空状态首次导入与激活模型被移除的回退外，
/// 不切换当前激活的供应商或模型。
pub fn import(app: &AppHandle, config: &str) -> Result<ProvidersImportResult> {
    let records = parse_provider_import(config)?;
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let state = load_state(app)?;
    let (state, added, updated) = merge_provider_import(state, records)?;
    let mut state = state;
    bump_revision(&mut state);
    save_state(app, &state)?;
    let list = render_list(state);
    Ok(ProvidersImportResult {
        providers: list.providers,
        default_model: list.default_model,
        active_provider_id: list.active_provider_id,
        added,
        updated,
    })
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
    if !provider.models.iter().any(|model| model == model_id)
        || provider.disabled_models.contains(model_id)
    {
        anyhow::bail!("供应商 {provider_id} 中找不到模型 {model_id}");
    }
    state.active_provider_id = Some(provider.id.clone());
    state.active_model_id = Some(model_id.to_string());
    bump_revision(&mut state);
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
            max_output_tokens: provider.max_output_tokens,
            chat_output_token_field: provider.chat_output_token_field,
            supports_vision: provider.supports_vision,
            reasoning_efforts: provider.reasoning_efforts,
            disabled_models: provider.disabled_models,
        })
        .collect();
    ProvidersListResult {
        providers,
        default_model,
        active_provider_id,
    }
}

/// 把桌面自有的 Provider 状态投影为 ZCode Provider Settings facade 合同。
///
/// 这里不引入第二份 Registry：所有字段都从 `ProviderState` 读取，Runtime 仍由
/// `replace_runtime_registry` 使用同一份状态构建。Builtin/账号事实在桌面 Host
/// 不存在，因此模板与 builtin 字段保持为空，不伪造官方账户能力。
pub(crate) fn facade_settings_view(app: &AppHandle) -> Result<Value> {
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let state = load_state(app)?;
    Ok(build_facade_settings_view(&state))
}

/// 投影模型选择候选；只暴露进入 Runtime 的 enabled 模型。
pub(crate) fn facade_model_selection_view(app: &AppHandle, input: Option<&Value>) -> Result<Value> {
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let state = load_state(app)?;
    Ok(build_facade_model_selection_view(&state, input))
}

pub(crate) fn facade_create_provider(app: &AppHandle, input: &Value) -> Result<Value> {
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let mut state = load_state(app)?;
    let object = input
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("createPersonalProvider 参数必须为对象"))?;
    let id = next_facade_provider_id(&state);
    let name = object
        .get("providerName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&id)
        .to_owned();
    let initial = object
        .get("initialConfig")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut record = ProviderRecord {
        id: id.clone(),
        name,
        template_id: match object.get("templateId") {
            None | Some(Value::Null) => None,
            Some(value) => {
                Some(validate_template_id(value.as_str().ok_or_else(|| {
                    anyhow::anyhow!("templateId 必须是字符串或 null")
                })?)?)
            }
        },
        // 空白创建仍必须能通过当前 providers.json 的严格 schema；用户随后可覆盖地址。
        base_url: FACADE_PLACEHOLDER_BASE_URL.to_owned(),
        models: Vec::new(),
        api_backend: "responses".to_owned(),
        api_key: None,
        context_windows: BTreeMap::new(),
        max_output_tokens: BTreeMap::new(),
        chat_output_token_field: ChatOutputTokenField::default(),
        supports_vision: BTreeMap::new(),
        reasoning_efforts: BTreeMap::new(),
        disabled_models: BTreeSet::new(),
        model_configs: BTreeMap::new(),
    };
    apply_provider_config(&mut record, &initial)?;
    if let Some(models) = initial.get("personalModelIds").and_then(Value::as_array) {
        let models = models
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("personalModelIds 必须是字符串数组"))
            })
            .collect::<Result<Vec<_>>>()?;
        let models = normalize_models(models)?;
        record.models = models.clone();
        record.supports_vision = models.iter().map(|model| (model.clone(), false)).collect();
    }
    if let Some(order) = initial.get("modelOrder").and_then(Value::as_array) {
        let order = order
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("modelOrder 必须是字符串数组"))
            })
            .collect::<Result<Vec<_>>>()?;
        if order.len() == record.models.len()
            && order.iter().all(|model| record.models.contains(model))
        {
            record.models = order;
        }
    }
    state.providers.push(record);
    if state.active_provider_id.is_none()
        && let Some(model) = state
            .providers
            .last()
            .and_then(|provider| provider.models.first())
    {
        state.active_provider_id = Some(id.clone());
        state.active_model_id = Some(model.clone());
    }
    repair_selection(&mut state);
    bump_revision(&mut state);
    save_state(app, &state)?;
    Ok(json!({"providerId": id, "view": build_facade_settings_view(&state)}))
}

pub(crate) fn facade_save_provider_overlay(
    app: &AppHandle,
    provider_id: &str,
    config: &Value,
    metadata: Option<&Value>,
) -> Result<Value> {
    mutate_facade_state(app, |state| {
        let provider = facade_provider_mut(state, provider_id)?;
        apply_provider_config(provider, config)?;
        if let Some(metadata) = metadata.and_then(Value::as_object) {
            if let Some(name) = metadata.get("providerName") {
                provider.name = name
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("providerName 必须是非空字符串"))?
                    .to_owned();
            }
            if let Some(template_id) = metadata.get("templateId") {
                provider.template_id = match template_id {
                    Value::Null => None,
                    value => {
                        Some(validate_template_id(value.as_str().ok_or_else(|| {
                            anyhow::anyhow!("templateId 必须是字符串或 null")
                        })?)?)
                    }
                };
            }
            if let Some(enabled) = metadata.get("enabled").and_then(Value::as_bool) {
                if enabled {
                    provider.disabled_models.clear();
                } else {
                    provider.disabled_models = provider.models.iter().cloned().collect();
                }
            }
        }
        Ok(())
    })
}

pub(crate) fn facade_delete_provider(app: &AppHandle, provider_id: &str) -> Result<Value> {
    mutate_facade_state(app, |state| {
        let id = validate_provider_id(provider_id)?;
        let before = state.providers.len();
        state.providers.retain(|provider| provider.id != id);
        if state.providers.len() == before {
            anyhow::bail!("找不到供应商 {id}");
        }
        repair_selection(state);
        Ok(())
    })
}

pub(crate) fn facade_reorder_providers(app: &AppHandle, provider_ids: &Value) -> Result<Value> {
    let ids = value_string_array(provider_ids, "providerIds")?;
    mutate_facade_state(app, |state| {
        let current = state
            .providers
            .iter()
            .map(|provider| provider.id.clone())
            .collect::<Vec<_>>();
        if ids.len() != current.len()
            || ids.iter().any(|id| !current.contains(id))
            || current.iter().any(|id| !ids.contains(id))
        {
            anyhow::bail!("providerIds 必须是当前供应商的完整排列");
        }
        let mut records = state.providers.clone();
        records.sort_by_key(|provider| {
            ids.iter()
                .position(|id| id == &provider.id)
                .expect("已校验供应商排列")
        });
        state.providers = records;
        Ok(())
    })
}

pub(crate) fn facade_reorder_models(
    app: &AppHandle,
    provider_id: &str,
    model_ids: &Value,
) -> Result<Value> {
    let ids = value_string_array(model_ids, "modelIds")?;
    mutate_facade_state(app, |state| {
        let provider = facade_provider_mut(state, provider_id)?;
        if ids.len() != provider.models.len()
            || ids.iter().any(|id| !provider.models.contains(id))
            || provider.models.iter().any(|id| !ids.contains(id))
        {
            anyhow::bail!("modelIds 必须是当前供应商模型的完整排列");
        }
        provider.models = ids.clone();
        Ok(())
    })
}

pub(crate) fn facade_add_model(
    app: &AppHandle,
    provider_id: &str,
    model_id: &str,
    config: &Value,
    _use_recommended_config: Option<bool>,
) -> Result<Value> {
    mutate_facade_state(app, |state| {
        let provider = facade_provider_mut(state, provider_id)?;
        let model_id = validate_model_id(model_id)?;
        if provider.models.iter().any(|model| model == &model_id) {
            anyhow::bail!("模型 {model_id} 已存在");
        }
        provider.models.push(model_id.clone());
        provider.supports_vision.insert(model_id.clone(), false);
        apply_model_config(provider, &model_id, config)?;
        Ok(())
    })
}

pub(crate) fn facade_rename_model(
    app: &AppHandle,
    provider_id: &str,
    current_model_id: &str,
    next_model_id: &str,
) -> Result<Value> {
    mutate_facade_state(app, |state| {
        let provider = facade_provider_mut(state, provider_id)?;
        let current = validate_model_id(current_model_id)?;
        let next = validate_model_id(next_model_id)?;
        let index = provider
            .models
            .iter()
            .position(|model| model == &current)
            .with_context(|| format!("找不到模型 {current}"))?;
        if current != next && provider.models.iter().any(|model| model == &next) {
            anyhow::bail!("模型 {next} 已存在");
        }
        provider.models[index] = next.clone();
        rename_model_key(&mut provider.context_windows, &current, &next);
        rename_model_key(&mut provider.max_output_tokens, &current, &next);
        rename_model_key(&mut provider.supports_vision, &current, &next);
        rename_model_key(&mut provider.reasoning_efforts, &current, &next);
        rename_model_key(&mut provider.model_configs, &current, &next);
        if provider.disabled_models.remove(&current) {
            provider.disabled_models.insert(next);
        }
        Ok(())
    })
}

pub(crate) fn facade_delete_model(
    app: &AppHandle,
    provider_id: &str,
    model_id: &str,
) -> Result<Value> {
    mutate_facade_state(app, |state| {
        let provider = facade_provider_mut(state, provider_id)?;
        let model_id = validate_model_id(model_id)?;
        let before = provider.models.len();
        provider.models.retain(|model| model != &model_id);
        if provider.models.len() == before {
            anyhow::bail!("找不到模型 {model_id}");
        }
        provider.context_windows.remove(&model_id);
        provider.max_output_tokens.remove(&model_id);
        provider.supports_vision.remove(&model_id);
        provider.reasoning_efforts.remove(&model_id);
        provider.disabled_models.remove(&model_id);
        provider.model_configs.remove(&model_id);
        repair_selection(state);
        Ok(())
    })
}

pub(crate) fn facade_save_model_draft(app: &AppHandle, input: &Value) -> Result<Value> {
    let object = input
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("savePersonalModelDraft 参数必须为对象"))?;
    let provider_id = required_value_string(object.get("providerId"), "providerId")?;
    let original = required_value_string(object.get("originalModelId"), "originalModelId")?;
    let next = required_value_string(object.get("nextModelId"), "nextModelId")?;
    let expected_revision = object.get("basedOnRevision").and_then(Value::as_u64);
    let config = object
        .get("personalConfig")
        .cloned()
        .unwrap_or_else(|| json!({}));
    mutate_facade_state(app, |state| {
        if let Some(expected) = expected_revision
            && facade_revision(state) != expected
        {
            anyhow::bail!("Provider Settings revision conflict");
        }
        let provider = facade_provider_mut(state, &provider_id)?;
        let original = validate_model_id(&original)?;
        let next = validate_model_id(&next)?;
        let index = provider
            .models
            .iter()
            .position(|model| model == &original)
            .with_context(|| format!("找不到模型 {original}"))?;
        if original != next && provider.models.iter().any(|model| model == &next) {
            anyhow::bail!("模型 {next} 已存在");
        }
        provider.models[index] = next.clone();
        if original != next {
            rename_model_key(&mut provider.context_windows, &original, &next);
            rename_model_key(&mut provider.max_output_tokens, &original, &next);
            rename_model_key(&mut provider.supports_vision, &original, &next);
            rename_model_key(&mut provider.reasoning_efforts, &original, &next);
            rename_model_key(&mut provider.model_configs, &original, &next);
            if provider.disabled_models.remove(&original) {
                provider.disabled_models.insert(next.clone());
            }
        }
        apply_model_config(provider, &next, &config)?;
        Ok(())
    })
}

pub(crate) fn facade_set_model_enabled(
    app: &AppHandle,
    provider_id: &str,
    model_id: &str,
    enabled: bool,
) -> Result<Value> {
    mutate_facade_state(app, |state| {
        let provider = facade_provider_mut(state, provider_id)?;
        let model_id = validate_model_id(model_id)?;
        if !provider.models.iter().any(|model| model == &model_id) {
            anyhow::bail!("找不到模型 {model_id}");
        }
        if enabled {
            provider.disabled_models.remove(&model_id);
        } else {
            provider.disabled_models.insert(model_id.clone());
        }
        if state.active_provider_id.as_deref() == Some(provider_id)
            && state.active_model_id.as_deref() == Some(model_id.as_str())
            && !enabled
        {
            repair_selection(state);
        }
        Ok(())
    })
}

pub(crate) fn facade_resolve_model_config(app: &AppHandle, input: &Value) -> Result<Value> {
    let object = input
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("resolveModelConfig 参数必须为对象"))?;
    let provider_id = required_value_string(object.get("providerId"), "providerId")?;
    let model_id = required_value_string(
        object
            .get("modelId")
            .or_else(|| object.get("originalModelId")),
        "modelId",
    )?;
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let state = load_state(app)?;
    let provider = state
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .with_context(|| format!("找不到供应商 {provider_id}"))?;
    if !provider.models.iter().any(|model| model == &model_id) {
        anyhow::bail!("找不到模型 {provider_id}/{model_id}");
    }
    let inherited = complete_model_config(provider, &model_id, None, false);
    let effective = if let Some(personal) = object.get("personalConfig") {
        validate_model_overlay(personal)?;
        complete_model_config(provider, &model_id, Some(personal), false)
    } else {
        complete_model_config(
            provider,
            &model_id,
            provider.model_configs.get(&model_id),
            provider.disabled_models.contains(&model_id),
        )
    };
    Ok(json!({"inheritedConfig": inherited, "effectiveConfig": effective, "issues": []}))
}

/// 变更成功后统一修复默认选择，并通过同一状态生成 Settings view。
fn mutate_facade_state(
    app: &AppHandle,
    mutate: impl FnOnce(&mut ProviderState) -> Result<()>,
) -> Result<Value> {
    let _guard = PROVIDER_IO_LOCK.lock().expect("供应商配置读写锁已损坏");
    let mut state = load_state(app)?;
    mutate(&mut state)?;
    repair_selection(&mut state);
    bump_revision(&mut state);
    save_state(app, &state)?;
    Ok(build_facade_settings_view(&state))
}

fn build_facade_settings_view(state: &ProviderState) -> Value {
    let providers = state
        .providers
        .iter()
        .map(|provider| {
            let provider_executable = provider_is_executable(provider);
            let models = provider
                .models
                .iter()
                .map(|model| {
                    let disabled = provider.disabled_models.contains(model);
                    let personal = provider.model_configs.get(model);
                    let builtin = complete_model_config(provider, model, None, false);
                    let effective = complete_model_config(
                        provider,
                        model,
                        personal,
                        disabled,
                    );
                    let mut model_view = json!({
                        "kind": "candidate",
                        "modelId": model,
                        "builtin": false,
                        "effectiveBuiltinConfig": builtin,
                        "effectiveConfig": effective,
                        "enabled": !disabled,
                        "executable": provider_executable && !disabled,
                        "selectable": provider_executable && !disabled,
                        "issues": []
                    });
                    if let Some(personal) = personal {
                        // 空 reasoning_efforts 是用户的显式禁用事实；源 facade 的
                        // strict schema 不接受空 values，因此只在展示层省略该 optionSpec，
                        // 磁盘记录仍保留空数组以便下一次编辑继续表达禁用。
                        model_view["personalExactConfig"] =
                            display_model_overlay(personal);
                        model_view["useRecommendedConfig"] = Value::Bool(false);
                    }
                    model_view
                })
                .collect::<Vec<_>>();
            let effective_config = provider_config_value(provider);
            let mut provider_view = json!({
                "providerId": provider.id,
                "providerName": provider.name,
                "enabled": provider.models.iter().any(|model| !provider.disabled_models.contains(model)),
                "executable": provider_executable,
                "personalConfig": effective_config,
                "effectiveConfig": effective_config,
                "issues": provider_issues(provider),
                "models": models
            });
            if let Some(template_id) = provider.template_id.as_deref() {
                provider_view["templateId"] = Value::String(template_id.to_owned());
            }
            provider_view
        })
        .collect::<Vec<_>>();
    json!({
        "revision": facade_revision(state),
        "providerTemplates": [],
        "providerOrder": state.providers.iter().map(|provider| provider.id.clone()).collect::<Vec<_>>(),
        "providers": providers
    })
}

fn build_facade_model_selection_view(state: &ProviderState, input: Option<&Value>) -> Value {
    let providers = state
        .providers
        .iter()
        .filter(|provider| provider_is_executable(provider))
        .map(|provider| {
            let models = provider
                .models
                .iter()
                .filter(|model| !provider.disabled_models.contains(*model))
                .map(|model| {
                    json!({
                        "modelId": model,
                        "config": complete_model_config(provider, model, provider.model_configs.get(model), false)
                    })
                })
                .collect::<Vec<_>>();
            let mut provider_view = json!({
                "providerId": provider.id,
                "providerName": provider.name,
                "config": provider_config_value(provider),
                "models": models
            });
            if let Some(template_id) = provider.template_id.as_deref() {
                provider_view["templateId"] = Value::String(template_id.to_owned());
            }
            provider_view
        })
        .collect::<Vec<_>>();
    let preferred = preferred_selection(state, &providers);
    let mut result = json!({
        "revision": facade_revision(state),
        "providers": providers
    });
    if let Some(selection) = preferred {
        result["preferredSelection"] = selection;
    }
    if let Some(input) = input.and_then(|value| value.get("selection")) {
        if input.is_null() {
            result["effectiveSelection"] = Value::Null;
            result["selectionIssue"] = Value::String("selection-missing".to_owned());
        } else if let Some(selection) = input.as_object() {
            let provider_id = selection.get("providerId").and_then(Value::as_str);
            let model_id = selection.get("modelId").and_then(Value::as_str);
            let provider = provider_id.and_then(|id| {
                providers
                    .iter()
                    .find(|provider| provider.get("providerId").and_then(Value::as_str) == Some(id))
            });
            let model = provider.and_then(|provider| {
                model_id.and_then(|id| {
                    provider
                        .get("models")
                        .and_then(Value::as_array)
                        .and_then(|models| {
                            models.iter().find(|model| {
                                model.get("modelId").and_then(Value::as_str) == Some(id)
                            })
                        })
                })
            });
            if provider.is_some() && model.is_some() {
                let reasoning = selection
                    .get("options")
                    .and_then(Value::as_object)
                    .and_then(|options| options.get("reasoningLevel"))
                    .and_then(Value::as_str);
                let supported_values = model
                    .and_then(|model| model.pointer("/config/optionSpecs/reasoningLevel/values"))
                    .and_then(Value::as_array);
                let supported = supported_values.map_or_else(
                    || reasoning.is_none(),
                    |values| {
                        reasoning.is_some_and(|reasoning| {
                            values.iter().any(|value| value.as_str() == Some(reasoning))
                        })
                    },
                );
                // Source facade 保留可识别的 provider/model 身份；档位缺失或失效
                // 只删除 options 并报告 issue，等待 UI 重新选择档位。
                let mut effective = Map::from_iter([
                    (
                        "providerId".to_owned(),
                        Value::String(provider_id.unwrap().to_owned()),
                    ),
                    (
                        "modelId".to_owned(),
                        Value::String(model_id.unwrap().to_owned()),
                    ),
                ]);
                if supported {
                    if let Some(options) = selection.get("options") {
                        effective.insert("options".to_owned(), options.clone());
                    }
                    result["effectiveSelection"] = Value::Object(effective);
                } else {
                    result["effectiveSelection"] = Value::Object(effective);
                    let issue = if reasoning.is_none() {
                        "reasoning-level-missing"
                    } else {
                        "reasoning-level-not-supported"
                    };
                    result["selectionIssue"] = Value::String(issue.to_owned());
                }
            } else {
                result["effectiveSelection"] = Value::Null;
                let issue = if provider.is_none() {
                    "provider-not-found"
                } else {
                    "model-not-found"
                };
                result["selectionIssue"] = Value::String(issue.to_owned());
            }
        }
    }
    result
}

fn preferred_selection(state: &ProviderState, providers: &[Value]) -> Option<Value> {
    let complete = |provider_id: &str, model_id: &str| {
        let provider = providers.iter().find(|provider| {
            provider.get("providerId").and_then(Value::as_str) == Some(provider_id)
        })?;
        let model = provider
            .get("models")
            .and_then(Value::as_array)?
            .iter()
            .find(|model| model.get("modelId").and_then(Value::as_str) == Some(model_id))?;
        let reasoning = model
            .pointer("/config/optionSpecs/reasoningLevel/values")
            .and_then(Value::as_array)
            .and_then(|values| values.last())
            .and_then(Value::as_str);
        // 推理档位是可选能力。用户显式关闭档位后仍必须能选择已配置的模型；
        // 不能把缺少 reasoningLevel 当作整条模型选择不可用。
        Some(json!({
            "providerId": provider_id,
            "modelId": model_id,
            "options": reasoning.map_or_else(|| json!({}), |level| json!({"reasoningLevel": level}))
        }))
    };
    if let (Some(provider_id), Some(model_id)) = (
        state.active_provider_id.as_deref(),
        state.active_model_id.as_deref(),
    ) && let Some(selection) = complete(provider_id, model_id)
    {
        return Some(selection);
    }
    providers.iter().find_map(|provider| {
        let provider_id = provider.get("providerId")?.as_str()?;
        let model_id = provider
            .get("models")?
            .as_array()?
            .first()?
            .get("modelId")?
            .as_str()?;
        complete(provider_id, model_id)
    })
}

fn provider_config_value(provider: &ProviderRecord) -> Value {
    let access = match &provider.api_key {
        Some(api_key) => json!({"type": "api-key", "apiKey": api_key}),
        None => json!({"type": "api-key"}),
    };
    json!({
        "group": "standard-personal",
        "access": access,
        "api": {
            "type": source_api_type(&provider.api_backend),
            "baseUrl": provider.base_url
        },
        "personalModelIds": provider.models,
        "modelOrder": provider.models,
        "visibility": "visible"
    })
}

fn source_api_type(api_backend: &str) -> &'static str {
    match api_backend {
        "messages" => "anthropic-messages",
        "chat_completions" => "openai-chat-completions",
        _ => "openai-responses",
    }
}

fn complete_model_config(
    provider: &ProviderRecord,
    model: &str,
    overlay: Option<&Value>,
    disabled: bool,
) -> Value {
    let context_window = provider
        .context_windows
        .get(model)
        .copied()
        .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS);
    let vision = provider
        .supports_vision
        .get(model)
        .copied()
        .unwrap_or(false);
    let explicit_reasoning = provider.reasoning_efforts.get(model);
    let efforts = explicit_reasoning.cloned().unwrap_or_else(|| {
        REASONING_EFFORT_IDS
            .iter()
            .map(|id| (*id).to_owned())
            .collect()
    });
    let max_output = provider
        .max_output_tokens
        .get(model)
        .copied()
        .unwrap_or(128_000);
    let mut option_specs = json!({
        "maxOutputTokens": {
            "max": max_output,
            "map": "{\"max_output_tokens\": maxOutputTokens}"
        }
    });
    if !explicit_reasoning.is_some_and(Vec::is_empty) {
        option_specs["reasoningLevel"] = json!({
            "values": efforts,
            "map": "{\"reasoning_effort\": reasoningLevel}"
        });
    }
    let mut value = json!({
        "enabled": !disabled,
        "properties": {
            "requiresMfjsToolSchema": false,
            "contextWindow": context_window,
            "inputFormat": {
                "supportsText": true,
                "supportsImage": vision,
                "supportsVideo": false,
                "supportsAudio": false,
                "supportsPdf": false
            },
            "outputFormat": {"supportsText": true},
            "supportsToolCall": true,
            "supportsJsonSchemaOutput": true,
            "supportsNativeWebSearch": false,
            "supportsMidConversationSystem": true
        },
        "optionSpecs": option_specs
    });
    if let Some(overlay) = overlay {
        let display_overlay =
            display_model_overlay_for_empty_reasoning(overlay, explicit_reasoning);
        merge_sparse_json(&mut value, &display_overlay);
    }
    if disabled {
        value["enabled"] = Value::Bool(false);
    }
    value
}

/// facade 的 strict model-config schema 不允许 `reasoningLevel.values: []`。
/// 持久化层仍保存空数组，但跨到前端时省略该 optionSpec，表示模型不开放推理档位。
fn display_model_overlay(overlay: &Value) -> Value {
    let mut display = overlay.clone();
    let Some(option_specs) = display
        .get_mut("optionSpecs")
        .and_then(Value::as_object_mut)
    else {
        return display;
    };
    let is_empty_reasoning = option_specs
        .get("reasoningLevel")
        .and_then(Value::as_object)
        .and_then(|reasoning| reasoning.get("values"))
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    if is_empty_reasoning {
        option_specs.remove("reasoningLevel");
    }
    display
}

fn display_model_overlay_for_empty_reasoning(
    overlay: &Value,
    explicit_reasoning: Option<&Vec<String>>,
) -> Value {
    let mut display = display_model_overlay(overlay);
    if !explicit_reasoning.is_some_and(Vec::is_empty) {
        return display;
    }
    let Some(option_specs) = display
        .get_mut("optionSpecs")
        .and_then(Value::as_object_mut)
    else {
        return display;
    };
    // ProviderRecord 的显式空数组是权威禁用标记，不能被旧 overlay 中的档位覆盖。
    option_specs.remove("reasoningLevel");
    display
}

fn merge_sparse_json(base: &mut Value, overlay: &Value) {
    let (Some(base_object), Some(overlay_object)) = (base.as_object_mut(), overlay.as_object())
    else {
        *base = overlay.clone();
        return;
    };
    for (key, value) in overlay_object {
        match base_object.get_mut(key) {
            Some(existing) if existing.is_object() && value.is_object() => {
                merge_sparse_json(existing, value)
            }
            _ => {
                base_object.insert(key.clone(), value.clone());
            }
        }
    }
}

fn provider_is_executable(provider: &ProviderRecord) -> bool {
    !is_facade_placeholder_url(&provider.base_url)
        && !provider.models.is_empty()
        && provider
            .models
            .iter()
            .any(|model| !provider.disabled_models.contains(model))
        && validate_base_url(&provider.base_url).is_ok()
        && validate_api_backend(&provider.api_backend).is_ok()
        && validate_exact_endpoint(&provider.base_url, &provider.api_backend).is_ok()
}

fn is_facade_placeholder_url(base_url: &str) -> bool {
    base_url == FACADE_PLACEHOLDER_BASE_URL
}

fn provider_issues(provider: &ProviderRecord) -> Value {
    if provider_is_executable(provider) {
        Value::Array(Vec::new())
    } else {
        json!([{
            "code": "invalid-config",
            "path": ["providers", provider.id],
            "message": "Provider 配置尚未进入可执行 Runtime"
        }])
    }
}

fn facade_revision(state: &ProviderState) -> u64 {
    state.revision
}

/// 在一次成功的状态变更写入前分配下一个 facade revision。
fn bump_revision(state: &mut ProviderState) {
    state.revision = state.revision.saturating_add(1);
}

fn apply_provider_config(record: &mut ProviderRecord, config: &Value) -> Result<()> {
    let Some(object) = config.as_object() else {
        anyhow::bail!("Provider config 必须是对象");
    };
    if let Some(api) = object.get("api").and_then(Value::as_object) {
        if let Some(api_type) = api.get("type").and_then(Value::as_str) {
            record.api_backend = match api_type {
                "anthropic-messages" => "messages",
                "openai-chat-completions" => "chat_completions",
                "openai-responses" => "responses",
                other => anyhow::bail!("不支持的 Provider API 类型：{other}"),
            }
            .to_owned();
        }
        if let Some(base_url) = api.get("baseUrl").and_then(Value::as_str) {
            record.base_url = base_url.trim().to_owned();
        }
    }
    if let Some(access) = object.get("access").and_then(Value::as_object) {
        if let Some(access_type) = access.get("type").and_then(Value::as_str)
            && access_type != "api-key"
        {
            anyhow::bail!("桌面 Provider 仅支持 api-key 访问方式");
        }
        if let Some(api_key) = access.get("apiKey") {
            record.api_key = match api_key {
                Value::Null => None,
                Value::String(value) => Some(validate_secret(value)?.to_owned()),
                _ => anyhow::bail!("access.apiKey 必须是字符串或 null"),
            };
        }
    }
    Ok(())
}

fn apply_model_config(record: &mut ProviderRecord, model: &str, config: &Value) -> Result<()> {
    validate_model_overlay(config)?;
    let object = config
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Model config 必须是对象"))?;
    record
        .model_configs
        .insert(model.to_owned(), config.clone());
    if let Some(enabled) = object.get("enabled").and_then(Value::as_bool) {
        if enabled {
            record.disabled_models.remove(model);
        } else {
            record.disabled_models.insert(model.to_owned());
        }
    }
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        if let Some(context) = properties.get("contextWindow").and_then(Value::as_u64) {
            record.context_windows.insert(model.to_owned(), context);
        }
        if let Some(image) = properties
            .get("inputFormat")
            .and_then(Value::as_object)
            .and_then(|format| format.get("supportsImage"))
            .and_then(Value::as_bool)
        {
            record.supports_vision.insert(model.to_owned(), image);
        }
    }
    if let Some(options) = object.get("optionSpecs").and_then(Value::as_object) {
        if let Some(values) = options
            .get("reasoningLevel")
            .and_then(Value::as_object)
            .and_then(|spec| spec.get("values"))
            .and_then(Value::as_array)
        {
            let efforts = values
                .iter()
                .map(|value| value.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| anyhow::anyhow!("reasoningLevel.values 必须是字符串数组"))?;
            record.reasoning_efforts.insert(model.to_owned(), efforts);
        }
        if let Some(max) = options
            .get("maxOutputTokens")
            .and_then(Value::as_object)
            .and_then(|spec| spec.get("max"))
            .and_then(Value::as_u64)
        {
            let max = u32::try_from(max).context("maxOutputTokens.max 超过 u32 范围")?;
            record.max_output_tokens.insert(model.to_owned(), max);
        }
    }
    Ok(())
}

/// 保存前收窄到 ZCode ModelConfigObject 的稀疏字段集合，避免未知字段进入
/// Settings View 后被前端 strict schema 拒绝。
fn validate_model_overlay(config: &Value) -> Result<()> {
    let object = config
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Model config 必须是对象"))?;
    validate_object_keys(object, &["enabled", "properties", "optionSpecs"], "model")?;
    validate_optional_bool(object, "enabled", "model.enabled")?;

    if let Some(properties) = object.get("properties") {
        let Some(properties) = properties.as_object() else {
            if properties.is_null() {
                return validate_model_option_specs(object);
            }
            anyhow::bail!("model.properties 必须是对象或 null");
        };
        validate_object_keys(
            properties,
            &[
                "requiresMfjsToolSchema",
                "contextWindow",
                "inputFormat",
                "outputFormat",
                "supportsToolCall",
                "supportsJsonSchemaOutput",
                "supportsNativeWebSearch",
                "supportsMidConversationSystem",
            ],
            "model.properties",
        )?;
        for key in [
            "requiresMfjsToolSchema",
            "supportsToolCall",
            "supportsJsonSchemaOutput",
            "supportsNativeWebSearch",
            "supportsMidConversationSystem",
        ] {
            validate_optional_bool(properties, key, &format!("model.properties.{key}"))?;
        }
        validate_optional_positive_u64(
            properties,
            "contextWindow",
            "model.properties.contextWindow",
        )?;
        validate_model_format(
            properties,
            "inputFormat",
            &[
                "supportsText",
                "supportsImage",
                "supportsVideo",
                "supportsAudio",
                "supportsPdf",
            ],
        )?;
        validate_model_format(properties, "outputFormat", &["supportsText"])?;
    }
    validate_model_option_specs(object)
}

fn validate_model_option_specs(object: &Map<String, Value>) -> Result<()> {
    let Some(option_specs) = object.get("optionSpecs") else {
        return Ok(());
    };
    let Some(option_specs) = option_specs.as_object() else {
        if option_specs.is_null() {
            return Ok(());
        }
        anyhow::bail!("model.optionSpecs 必须是对象或 null");
    };
    validate_object_keys(
        option_specs,
        &["reasoningLevel", "maxOutputTokens"],
        "model.optionSpecs",
    )?;
    if let Some(reasoning) = option_specs.get("reasoningLevel") {
        let Some(reasoning) = reasoning.as_object() else {
            if reasoning.is_null() {
                return Ok(());
            }
            anyhow::bail!("model.optionSpecs.reasoningLevel 必须是对象或 null");
        };
        validate_object_keys(
            reasoning,
            &["values", "map"],
            "model.optionSpecs.reasoningLevel",
        )?;
        if let Some(values) = reasoning.get("values") {
            let Some(values) = values.as_array() else {
                if values.is_null() {
                    return Ok(());
                }
                anyhow::bail!("reasoningLevel.values 必须是字符串数组或 null");
            };
            let mut seen = BTreeSet::new();
            for value in values {
                let value = value
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| anyhow::anyhow!("reasoningLevel.values 必须是非空字符串数组"))?;
                if !seen.insert(value.to_owned()) {
                    anyhow::bail!("reasoningLevel.values 不能重复");
                }
            }
        }
        validate_optional_non_empty_string(reasoning, "map", "reasoningLevel.map")?;
    }
    if let Some(max_output) = option_specs.get("maxOutputTokens") {
        let Some(max_output) = max_output.as_object() else {
            if max_output.is_null() {
                return Ok(());
            }
            anyhow::bail!("model.optionSpecs.maxOutputTokens 必须是对象或 null");
        };
        validate_object_keys(
            max_output,
            &["max", "map"],
            "model.optionSpecs.maxOutputTokens",
        )?;
        validate_optional_positive_u64(max_output, "max", "maxOutputTokens.max")?;
        validate_optional_non_empty_string(max_output, "map", "maxOutputTokens.map")?;
    }
    Ok(())
}

fn validate_model_format(
    properties: &Map<String, Value>,
    field: &str,
    allowed: &[&str],
) -> Result<()> {
    let Some(format) = properties.get(field) else {
        return Ok(());
    };
    let Some(format) = format.as_object() else {
        if format.is_null() {
            return Ok(());
        }
        anyhow::bail!("model.properties.{field} 必须是对象或 null");
    };
    validate_object_keys(format, allowed, &format!("model.properties.{field}"))?;
    for key in allowed {
        validate_optional_bool(format, key, &format!("model.properties.{field}.{key}"))?;
    }
    Ok(())
}

fn validate_object_keys(object: &Map<String, Value>, allowed: &[&str], path: &str) -> Result<()> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        anyhow::bail!("{path} 包含未知字段 {key}");
    }
    Ok(())
}

fn validate_optional_bool(object: &Map<String, Value>, key: &str, path: &str) -> Result<()> {
    if let Some(value) = object.get(key)
        && !value.is_null()
        && !value.is_boolean()
    {
        anyhow::bail!("{path} 必须是布尔值或 null");
    }
    Ok(())
}

fn validate_optional_positive_u64(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<()> {
    if let Some(value) = object.get(key) {
        if value.is_null() {
            return Ok(());
        }
        if value.as_u64().is_none_or(|value| value == 0) {
            anyhow::bail!("{path} 必须是正整数或 null");
        }
    }
    Ok(())
}

fn validate_optional_non_empty_string(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<()> {
    if let Some(value) = object.get(key)
        && value.as_str().is_none_or(|value| value.trim().is_empty())
        && !value.is_null()
    {
        anyhow::bail!("{path} 必须是非空字符串或 null");
    }
    Ok(())
}

fn facade_provider_mut<'a>(
    state: &'a mut ProviderState,
    provider_id: &str,
) -> Result<&'a mut ProviderRecord> {
    let id = validate_provider_id(provider_id)?;
    state
        .providers
        .iter_mut()
        .find(|provider| provider.id == id)
        .with_context(|| format!("找不到供应商 {id}"))
}

fn next_facade_provider_id(state: &ProviderState) -> String {
    let mut index = 1u32;
    loop {
        let candidate = format!("personal-provider-{index}");
        if !state
            .providers
            .iter()
            .any(|provider| provider.id == candidate)
        {
            return candidate;
        }
        index = index.saturating_add(1);
    }
}

fn required_value_string(value: Option<&Value>, field: &str) -> Result<String> {
    let value = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{field} 必须是非空字符串"))?;
    Ok(value.to_owned())
}

fn value_string_array(value: &Value, field: &str) -> Result<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{field} 必须是字符串数组"))?
        .iter()
        .map(|value| required_value_string(Some(value), field))
        .collect()
}

fn validate_model_id(raw: &str) -> Result<String> {
    let value = raw.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        anyhow::bail!("模型标识不能为空或包含控制字符");
    }
    Ok(value.to_owned())
}

fn rename_model_key<T>(map: &mut BTreeMap<String, T>, current: &str, next: &str) {
    if let Some(value) = map.remove(current) {
        map.insert(next.to_owned(), value);
    }
}

fn repair_selection(state: &mut ProviderState) {
    let is_enabled = |provider: &ProviderRecord, model: &str| {
        provider.models.iter().any(|candidate| candidate == model)
            && !provider.disabled_models.contains(model)
    };
    let selected = state
        .active_provider_id
        .as_deref()
        .and_then(|provider_id| {
            state
                .providers
                .iter()
                .find(|provider| provider.id == provider_id)
        })
        .and_then(|provider| {
            state
                .active_model_id
                .as_deref()
                .filter(|model| is_enabled(provider, model))
                .map(|model| (provider.id.clone(), model.to_owned()))
                .or_else(|| {
                    provider
                        .models
                        .iter()
                        .find(|model| is_enabled(provider, model))
                        .map(|model| (provider.id.clone(), model.clone()))
                })
        })
        .or_else(|| {
            state.providers.iter().find_map(|provider| {
                provider
                    .models
                    .iter()
                    .find(|model| is_enabled(provider, model))
                    .map(|model| (provider.id.clone(), model.clone()))
            })
        });
    match selected {
        Some((provider_id, model_id)) => {
            state.active_provider_id = Some(provider_id);
            state.active_model_id = Some(model_id);
        }
        None => {
            state.active_provider_id = None;
            state.active_model_id = None;
        }
    }
}

/// 读取供应商状态文件。
fn load_state(app: &AppHandle) -> Result<ProviderState> {
    let path = state_path(app)?;
    load_state_from_path(&path)
}

/// 从明确路径读取供应商状态；只有文件不存在时才返回当前空状态。
///
/// 磁盘配置可能来自其他版本：未知或已移除字段、以及指向已删除模型的模型级
/// 配置一律忽略并记入诊断日志，不能让整份配置无法加载。结构性错误（非 JSON、
/// schema/版本不符、记录自身非法）仍然失败关闭。
fn load_state_from_path(path: &Path) -> Result<ProviderState> {
    let Some(bytes) =
        crate::storage::read_private_bytes_bounded(path, MAX_PROVIDER_CONFIG_BYTES, "供应商配置")?
    else {
        return Ok(ProviderState::default());
    };
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("供应商配置格式无效：{}", path.display()))?;
    let mut warnings = unknown_field_warnings(&value)
        .map_err(|message| anyhow::anyhow!("供应商配置无效：{}：{}", message, path.display()))?;
    let file: ProviderFile = serde_json::from_value(value)
        .with_context(|| format!("供应商配置格式无效：{}", path.display()))?;
    let mut state = file
        .into_state()
        .with_context(|| format!("供应商配置 schema 无效：{}", path.display()))?;
    warnings.append(&mut normalize_loaded_state(&mut state));
    for warning in &warnings {
        // 只记录不阻断：用户需要在日志里看到跳过了什么，而不是启动后无法发消息。
        tracing::warn!(
            target: "keencode_diagnostics",
            component = "providers.load",
            path = %path.display(),
            "{warning}"
        );
    }
    validate_state(&state)?;
    Ok(state)
}

/// 当前供应商配置外壳的已知顶层字段。
const PROVIDER_FILE_KEYS: &[&str] = &[
    "schema",
    "version",
    "activeProviderId",
    "activeModelId",
    "revision",
    "providers",
];

/// 当前供应商记录的已知字段。
const PROVIDER_RECORD_KEYS: &[&str] = &[
    "id",
    "name",
    "baseUrl",
    "models",
    "apiBackend",
    "apiKey",
    "contextWindows",
    "maxOutputTokens",
    "chatOutputTokenField",
    "supportsVision",
    "reasoningEfforts",
    "disabledModels",
    "modelConfigs",
];

/// 找出配置中未知或已移除的字段；只报告字段名，不参与解析。
///
/// 已移除字段按警告忽略；但与已知字段仅大小写不同的键几乎必然是拼写错误
/// （例如 `apikey` 会让认证静默丢失），这类键返回 Err 阻断加载。
fn unknown_field_warnings(value: &Value) -> Result<Vec<String>, String> {
    let Some(object) = value.as_object() else {
        return Ok(Vec::new());
    };
    let (typo, unknown) =
        partition_unknown_keys(object.keys().map(String::as_str), PROVIDER_FILE_KEYS);
    if let Some(typo) = typo.first() {
        return Err(format!(
            "供应商配置字段 `{typo}` 与已知字段仅大小写不同，疑似拼写错误；请修正字段名后重试"
        ));
    }
    let mut warnings = Vec::new();
    if !unknown.is_empty() {
        warnings.push(format!(
            "供应商配置包含未知或已移除字段，已忽略：{}",
            unknown.join(", ")
        ));
    }
    let Some(records) = object.get("providers").and_then(Value::as_array) else {
        return Ok(warnings);
    };
    for record in records {
        let Some(record) = record.as_object() else {
            continue;
        };
        let (typo, unknown) =
            partition_unknown_keys(record.keys().map(String::as_str), PROVIDER_RECORD_KEYS);
        let id = record
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("<缺少 id>");
        if let Some(typo) = typo.first() {
            return Err(format!(
                "供应商 {id} 的字段 `{typo}` 与已知字段仅大小写不同，疑似拼写错误；请修正字段名后重试"
            ));
        }
        if !unknown.is_empty() {
            warnings.push(format!(
                "供应商 {id} 包含未知或已移除字段，已忽略：{}",
                unknown.join(", ")
            ));
        }
    }
    Ok(warnings)
}

/// 把未知键划分为「与已知字段仅大小写不同」与「其余未知/已移除字段」。
fn partition_unknown_keys<'a>(
    keys: impl Iterator<Item = &'a str>,
    known: &[&str],
) -> (Vec<&'a str>, Vec<&'a str>) {
    let mut typo = Vec::new();
    let mut unknown = Vec::new();
    for key in keys {
        if known.contains(&key) {
            continue;
        }
        if known.iter().any(|known| known.eq_ignore_ascii_case(key)) {
            typo.push(key);
        } else {
            unknown.push(key);
        }
    }
    (typo, unknown)
}

/// 把磁盘配置归一化为当前唯一结构。
///
/// 只做“丢弃已失效配置”的收敛：指向已删除模型的模型级条目、超出合法范围的
/// 手工值、以及指向已不存在供应商或模型的当前选择。返回值是需要记录的说明。
fn normalize_loaded_state(state: &mut ProviderState) -> Vec<String> {
    let mut warnings = Vec::new();
    for provider in &mut state.providers {
        let models = &provider.models;
        let dropped: Vec<String> = provider
            .context_windows
            .keys()
            .filter(|model| !models.iter().any(|item| item == *model))
            .cloned()
            .collect();
        for model in &dropped {
            provider.context_windows.remove(model);
        }
        if !dropped.is_empty() {
            warnings.push(format!(
                "供应商 {} 的上下文窗口配置指向已删除模型，已忽略：{}",
                provider.id,
                dropped.join(", ")
            ));
        }

        let out_of_range: Vec<String> = provider
            .context_windows
            .iter()
            .filter(|(_, window)| !(MIN_CONTEXT_WINDOW..=MAX_CONTEXT_WINDOW).contains(*window))
            .map(|(model, _)| model.clone())
            .collect();
        for model in &out_of_range {
            provider.context_windows.remove(model);
        }
        if !out_of_range.is_empty() {
            warnings.push(format!(
                "供应商 {} 的上下文窗口超出合法范围，已忽略：{}",
                provider.id,
                out_of_range.join(", ")
            ));
        }

        let stale_output: Vec<String> = provider
            .max_output_tokens
            .iter()
            .filter(|(model, value)| **value == 0 || !models.iter().any(|item| item == *model))
            .map(|(model, _)| model.clone())
            .collect();
        for model in &stale_output {
            provider.max_output_tokens.remove(model);
        }
        if !stale_output.is_empty() {
            warnings.push(format!(
                "供应商 {} 的输出预算配置无效或指向已删除模型，已忽略：{}",
                provider.id,
                stale_output.join(", ")
            ));
        }

        let stale_vision: Vec<String> = provider
            .supports_vision
            .keys()
            .filter(|model| !models.iter().any(|item| item == *model))
            .cloned()
            .collect();
        for model in &stale_vision {
            provider.supports_vision.remove(model);
        }
        // 已删除模型或不受运行时支持的档位不应阻止整个配置加载。
        let stale_efforts: Vec<String> = provider.reasoning_efforts.keys().cloned().collect();
        provider.reasoning_efforts.retain(|model, efforts| {
            models.iter().any(|item| item == model) && reasoning_efforts_in_order(efforts)
        });
        let dropped_efforts: Vec<_> = stale_efforts
            .into_iter()
            .filter(|model| !provider.reasoning_efforts.contains_key(model))
            .collect();
        if !dropped_efforts.is_empty() {
            warnings.push(format!(
                "供应商 {} 的推理档位配置无效或指向已删除模型，已忽略：{}",
                provider.id,
                dropped_efforts.join(", ")
            ));
        }

        let stale_model_configs: Vec<String> = provider
            .model_configs
            .keys()
            .filter(|model| !models.iter().any(|item| item == *model))
            .cloned()
            .collect();
        for model in &stale_model_configs {
            provider.model_configs.remove(model);
        }
        if !stale_model_configs.is_empty() {
            warnings.push(format!(
                "供应商 {} 的模型覆盖指向已删除模型，已忽略：{}",
                provider.id,
                stale_model_configs.join(", ")
            ));
        }
        let invalid_model_configs: Vec<String> = provider
            .model_configs
            .iter()
            .filter(|(_, config)| validate_model_overlay(config).is_err())
            .map(|(model, _)| model.clone())
            .collect();
        for model in &invalid_model_configs {
            provider.model_configs.remove(model);
        }
        if !invalid_model_configs.is_empty() {
            warnings.push(format!(
                "供应商 {} 的模型覆盖不符合 source schema，已忽略：{}",
                provider.id,
                invalid_model_configs.join(", ")
            ));
        }
        // 缺失的视觉能力按“不支持”补齐，与运行时读取时的默认值一致，
        // 避免新增模型后整份配置因缺少该字段而无法加载。
        let missing_vision: Vec<String> = models
            .iter()
            .filter(|model| !provider.supports_vision.contains_key(*model))
            .cloned()
            .collect();
        for model in &missing_vision {
            provider.supports_vision.insert(model.clone(), false);
        }
        if !stale_vision.is_empty() || !missing_vision.is_empty() {
            let mut parts = Vec::new();
            if !stale_vision.is_empty() {
                parts.push(format!("已忽略 {}", stale_vision.join(", ")));
            }
            if !missing_vision.is_empty() {
                parts.push(format!("按不支持补齐 {}", missing_vision.join(", ")));
            }
            warnings.push(format!(
                "供应商 {} 的视觉能力配置已收敛：{}",
                provider.id,
                parts.join("；")
            ));
        }

        let stale_disabled: Vec<String> = provider
            .disabled_models
            .iter()
            .filter(|model| !models.iter().any(|item| item == *model))
            .cloned()
            .collect();
        for model in &stale_disabled {
            provider.disabled_models.remove(model);
        }
        if !stale_disabled.is_empty() {
            warnings.push(format!(
                "供应商 {} 的禁用模型配置指向已删除模型，已忽略：{}",
                provider.id,
                stale_disabled.join(", ")
            ));
        }
    }

    match (
        state.providers.is_empty(),
        state.active_provider_id.clone(),
        state.active_model_id.clone(),
    ) {
        (true, None, None) => {}
        (true, _, _) => {
            state.active_provider_id = None;
            state.active_model_id = None;
            warnings.push("配置没有任何供应商，已清除当前供应商与模型选择".to_owned());
        }
        (false, provider_id, model_id) => {
            let found = provider_id
                .as_deref()
                .and_then(|id| state.providers.iter().find(|item| item.id == id));
            let provider = match found {
                Some(provider) => provider,
                None => {
                    let fallback = &state.providers[0];
                    warnings.push(format!("当前供应商不存在，已回退为 {}", fallback.id));
                    state.active_provider_id = Some(fallback.id.clone());
                    state.active_model_id = fallback.models.first().cloned();
                    repair_selection(state);
                    return warnings;
                }
            };
            if model_id.as_deref().is_some_and(|model| {
                provider.models.iter().any(|item| item == model)
                    && !provider.disabled_models.contains(model)
            }) {
                // 当前选择仍指向启用模型，无需修改。
            } else {
                let fallback = provider
                    .models
                    .iter()
                    .find(|model| !provider.disabled_models.contains(*model))
                    .cloned();
                warnings.push(format!(
                    "当前模型不属于供应商 {}，已回退为 {}",
                    provider.id,
                    fallback.as_deref().unwrap_or("<无可用模型>")
                ));
                state.active_model_id = fallback;
            }
        }
    }
    repair_selection(state);
    warnings
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
        if let Some(template_id) = provider.template_id.as_deref()
            && validate_template_id(template_id)? != template_id
        {
            anyhow::bail!("供应商 {} 的 templateId 不是规范格式", provider.id);
        }
        if validate_base_url(&provider.base_url)? != provider.base_url {
            anyhow::bail!("供应商 {} 的 API 地址不是规范格式", provider.id);
        }
        if let Err(error) = validate_exact_endpoint(&provider.base_url, &provider.api_backend) {
            anyhow::bail!("供应商 {} 的 API 地址不合规：{error}", provider.id);
        }
        if !provider.models.is_empty()
            && normalize_models(provider.models.clone())? != provider.models
        {
            anyhow::bail!(
                "供应商 {} 的模型列表包含空项、重复项或首尾空白",
                provider.id
            );
        }
        validate_context_windows(provider.context_windows.clone(), &provider.models)?;
        validate_max_output_tokens(provider.max_output_tokens.clone(), &provider.models)?;
        validate_supports_vision(provider.supports_vision.clone(), &provider.models)?;
        validate_reasoning_efforts(provider.reasoning_efforts.clone(), &provider.models)?;
        if provider
            .disabled_models
            .iter()
            .any(|model| !provider.models.iter().any(|candidate| candidate == model))
        {
            anyhow::bail!("供应商 {} 的禁用模型列表包含未知模型", provider.id);
        }
        if provider.model_configs.iter().any(|(model, config)| {
            !provider.models.iter().any(|candidate| candidate == model) || !config.is_object()
        }) {
            anyhow::bail!("供应商 {} 的模型覆盖无效", provider.id);
        }
        if validate_api_backend(&provider.api_backend)? != provider.api_backend {
            anyhow::bail!("供应商 {} 的协议类型不是规范格式", provider.id);
        }
        if provider.api_key.is_some() {
            validate_api_key(provider.api_key.as_deref())?;
        }
    }

    let has_enabled_model = state.providers.iter().any(|provider| {
        provider
            .models
            .iter()
            .any(|model| !provider.disabled_models.contains(model))
    });
    if !has_enabled_model {
        if state.active_provider_id.is_some() || state.active_model_id.is_some() {
            anyhow::bail!("没有可用模型时不能保存当前供应商或模型");
        }
        return Ok(());
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
            if provider.disabled_models.contains(model_id) {
                anyhow::bail!("当前模型 {model_id} 已禁用");
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

/// 输出预算必须为正数且只能关联已配置模型。
fn validate_max_output_tokens(
    values: BTreeMap<String, u32>,
    models: &[String],
) -> Result<BTreeMap<String, u32>> {
    for (model, value) in &values {
        if !models.contains(model) || *value == 0 {
            anyhow::bail!("模型 {model} 的最大输出 Token 配置无效");
        }
    }
    Ok(values)
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

/// 仅允许运行时可执行的档位，并保持用户指定的排序。
const REASONING_EFFORT_IDS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// 档位顺序同时决定前端滑块方向，导入数据必须保持从低到高且无重复。
fn reasoning_efforts_in_order(values: &[String]) -> bool {
    let mut previous = None;
    for value in values {
        let Some(index) = REASONING_EFFORT_IDS.iter().position(|id| *id == value) else {
            return false;
        };
        if previous.is_some_and(|last| index <= last) {
            return false;
        }
        previous = Some(index);
    }
    true
}

fn validate_reasoning_efforts(
    efforts: BTreeMap<String, Vec<String>>,
    models: &[String],
) -> Result<BTreeMap<String, Vec<String>>> {
    for (model, values) in &efforts {
        if !models.contains(model) || values.len() > REASONING_EFFORT_IDS.len() {
            anyhow::bail!("模型 {model} 的推理档位配置无效");
        }
        if !reasoning_efforts_in_order(values) {
            anyhow::bail!("模型 {model} 的推理档位不受支持、重复或顺序错误");
        }
    }
    Ok(efforts)
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

/// 校验可选的 Provider Template 身份；只保存身份，不复制模板或官方账户事实。
fn validate_template_id(raw: &str) -> Result<String> {
    let id = raw.trim();
    if id.is_empty() || id.len() > 128 {
        anyhow::bail!("templateId 长度必须为 1 到 128 个字符");
    }
    if !id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        anyhow::bail!("templateId 只能使用字母、数字、点、下划线和短横线");
    }
    Ok(id.to_owned())
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
        ProviderExportFile, ProviderFile, ProviderRecord, ProviderState, model_catalog_endpoint,
        validate_api_key, validate_base_url, validate_catalog_secret_scope,
        validate_context_windows, validate_exact_endpoint, validate_reasoning_efforts,
        validate_secret, validate_state,
    };
    use std::collections::BTreeMap;

    /// 人工配置只用于目录事务验收，不依赖本机供应商或真实凭据。
    fn models_editor_fixture() -> ProviderState {
        serde_json::from_value(serde_json::json!({
            "activeProviderId": "provider", "activeModelId": "old",
            "providers": [{"id": "provider", "name": "Fixture",
                "baseUrl": "https://api.example.com/v1", "apiBackend": "responses",
                "apiKey": "fixture-secret", "models": ["old", "keep"],
                "contextWindows": {"old": 32000, "keep": 64000},
                "maxOutputTokens": {"old": 1000, "keep": 2000},
                "supportsVision": {"old": false, "keep": true},
                "reasoningEfforts": {"old": ["low"], "keep": []}}]
        }))
        .unwrap()
    }

    /// 模型编辑必须原子保存、保留凭据和仍存在模型的能力，并同步默认选择。
    #[test]
    fn model_directory_editor_preserves_configuration_and_updates_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("providers.json");
        let before = models_editor_fixture();
        super::save_state_to_path(&path, &before).unwrap();
        let result = super::update_models_at_path(
            &path,
            vec!["provider::old".into(), "provider::keep".into()],
            vec!["provider::keep".into(), "provider::new".into()],
        )
        .unwrap();
        let after = super::load_state_from_path(&path).unwrap();
        let provider = &after.providers[0];
        assert_eq!(provider.api_key, before.providers[0].api_key);
        assert_eq!(provider.base_url, before.providers[0].base_url);
        assert_eq!(provider.api_backend, before.providers[0].api_backend);
        assert_eq!(provider.name, before.providers[0].name);
        assert_eq!(provider.models, ["keep", "new"]);
        assert_eq!(
            provider.context_windows,
            BTreeMap::from([("keep".into(), 64000)])
        );
        assert_eq!(
            provider.max_output_tokens,
            BTreeMap::from([("keep".into(), 2000)])
        );
        assert_eq!(
            provider.supports_vision,
            BTreeMap::from([("keep".into(), true), ("new".into(), false)])
        );
        assert_eq!(
            provider.reasoning_efforts,
            BTreeMap::from([("keep".into(), vec![])])
        );
        assert_eq!(result.default_model.as_deref(), Some("keep"));
        assert!(super::runtime_provider_registration(&result.providers[0]).is_ok());
    }

    /// 冲突、未知身份、重复或空目录失败时，不能部分改写配置文件。
    #[test]
    fn model_directory_editor_rejects_stale_and_invalid_input_without_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("providers.json");
        super::save_state_to_path(&path, &models_editor_fixture()).unwrap();
        let original = std::fs::read(&path).unwrap();
        let current = vec!["provider::old".into(), "provider::keep".into()];
        for entries in [
            vec![],
            vec!["missing::model"],
            vec!["provider::old", "provider::old"],
            vec!["unqualified"],
            vec!["provider:: old"],
            vec!["provider::bad\nmodel"],
        ] {
            assert!(
                super::update_models_at_path(
                    &path,
                    current.clone(),
                    entries.into_iter().map(str::to_owned).collect()
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        assert!(
            super::update_models_at_path(&path, vec!["provider::old".into()], current).is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn reasoning_efforts_validate_model_and_runtime_ids() {
        let models = vec!["test-model".to_owned()];
        let valid = BTreeMap::from([(
            "test-model".to_owned(),
            vec!["low".to_owned(), "high".to_owned(), "max".to_owned()],
        )]);
        assert_eq!(
            validate_reasoning_efforts(valid.clone(), &models).unwrap(),
            valid
        );
        assert!(
            validate_reasoning_efforts(
                BTreeMap::from([("test-model".to_owned(), vec![])]),
                &models
            )
            .is_ok()
        );
        for invalid in [vec!["unknown"], vec!["low", "low"], vec!["high", "low"]] {
            let values = invalid.into_iter().map(str::to_owned).collect();
            assert!(
                validate_reasoning_efforts(
                    BTreeMap::from([("test-model".to_owned(), values)]),
                    &models
                )
                .is_err()
            );
        }
        assert!(
            validate_reasoning_efforts(
                BTreeMap::from([("other".to_owned(), vec!["low".to_owned()])]),
                &models
            )
            .is_err()
        );
    }

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

    /// 供应商元数据接受 API Key 与模型推理档位，保存后原样往返。
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
            "supportsVision": {"test-model": false},
            "reasoningEfforts": {"test-model": ["low", "high", "max"]}
        });

        let record =
            serde_json::from_value::<ProviderRecord>(value.clone()).expect("应接受持久化密钥");
        assert_eq!(record.api_key.as_deref(), Some("persisted-key"));
        assert_eq!(
            record.reasoning_efforts["test-model"],
            ["low", "high", "max"]
        );
        let serialized = serde_json::to_value(&record).expect("配置可序列化");
        assert_eq!(
            serialized["reasoningEfforts"]["test-model"],
            value["reasoningEfforts"]["test-model"]
        );
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
            "contextWindows": {}
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
    }

    /// 未知或已移除字段必须被忽略并记录，而不是让整份配置无法加载。
    ///
    /// 旧版本写入的字段（如已移除的 `context1m`）会长期留在磁盘上，加载路径
    /// 必须降级继续运行；字段名进入诊断日志供定位。
    #[test]
    fn provider_config_ignores_unknown_fields_with_warnings() {
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
                "supportsVision": {"test-model": false},
                "context1m": {"test-model": true}
            }]
        });

        let warnings = super::unknown_field_warnings(&value)
            .expect("未知或已移除字段应按警告忽略，不应阻断加载");
        assert_eq!(
            warnings.len(),
            2,
            "顶层与记录级未知字段各一条：{warnings:?}"
        );
        assert!(warnings[0].contains("expiredTopLevelField"));
        assert!(warnings[1].contains("context1m"));

        let file = serde_json::from_value::<ProviderFile>(value).expect("未知字段必须被忽略");
        let state = file.into_state().expect("schema 与版本应有效");
        assert_eq!(state.providers.len(), 1);
        assert_eq!(state.providers[0].models, vec!["test-model".to_owned()]);
    }

    /// 与已知字段仅大小写不同的键几乎必然是拼写错误（如 `apikey` 会让
    /// 认证静默丢失），必须阻断加载而不是忽略。
    #[test]
    fn provider_config_rejects_case_only_typo_fields() {
        let value = serde_json::json!({
            "schema": "keencode/providers",
            "version": 1,
            "activeProviderId": "provider",
            "activeModelId": "test-model",
            "providers": [{
                "id": "provider",
                "name": "Provider",
                "baseUrl": "https://api.example.com/v1",
                "models": ["test-model"],
                "apiBackend": "responses",
                "apikey": "real-secret",
                "contextWindows": {},
                "supportsVision": {"test-model": false}
            }]
        });
        let error =
            super::unknown_field_warnings(&value).expect_err("仅大小写不同的字段名应阻断加载");
        assert!(error.contains("apikey"), "错误应指出问题字段：{error}");
    }

    /// 配置加载必须收敛已失效的模型级条目，并记录被丢弃的内容。
    #[test]
    fn provider_config_normalizes_stale_model_entries() {
        let value = serde_json::json!({
            "schema": "keencode/providers",
            "version": 1,
            "activeProviderId": "provider",
            "activeModelId": "removed-model",
            "providers": [{
                "id": "provider",
                "name": "Provider",
                "baseUrl": "https://api.example.com/v1",
                "models": ["test-model"],
                "apiBackend": "responses",
                "apiKey": null,
                "contextWindows": {"removed-model": 128000, "test-model": 99},
                "maxOutputTokens": {"removed-model": 128000, "test-model": 0},
                "supportsVision": {"removed-model": false}
            }]
        });

        let file = serde_json::from_value::<ProviderFile>(value).expect("应接受当前结构");
        let mut state = file.into_state().expect("schema 与版本应有效");
        let warnings = super::normalize_loaded_state(&mut state);
        let provider = &state.providers[0];

        assert!(
            provider.context_windows.is_empty(),
            "越界与失效窗口都应丢弃"
        );
        assert!(
            provider.max_output_tokens.is_empty(),
            "零值与失效预算都应丢弃"
        );
        assert_eq!(provider.supports_vision.get("test-model"), Some(&false));
        assert_eq!(provider.supports_vision.len(), 1, "失效模型键应被丢弃");
        assert_eq!(state.active_model_id.as_deref(), Some("test-model"));
        assert!(!warnings.is_empty(), "归一化必须留下可记录说明");
        assert!(
            super::validate_state(&state).is_ok(),
            "归一化后必须能通过校验"
        );
    }

    /// 归一化后的配置必须仍受严格校验约束：结构性错误不能借宽容加载蒙混过关。
    #[test]
    fn provider_config_still_rejects_structural_errors() {
        let value = serde_json::json!({
            "schema": "keencode/providers",
            "version": 1,
            "activeProviderId": "provider",
            "activeModelId": "test-model",
            "providers": [{
                "id": "provider",
                "name": "Provider",
                "baseUrl": "not-a-url",
                "models": ["test-model"],
                "apiBackend": "responses",
                "apiKey": null,
                "contextWindows": {},
                "supportsVision": {"test-model": false}
            }]
        });

        let mut state = serde_json::from_value::<ProviderFile>(value)
            .expect("字段形状仍应可解析")
            .into_state()
            .expect("schema 与版本应有效");
        super::normalize_loaded_state(&mut state);
        assert!(
            super::validate_state(&state).is_err(),
            "非法地址必须失败关闭"
        );
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
            revision: 0,
            providers: vec![ProviderRecord {
                id: "provider".to_string(),
                name: "Provider".to_string(),
                template_id: None,
                base_url: "https://api.example.com/v1".to_string(),
                models: vec!["test-model".to_string()],
                api_backend: "responses".to_string(),
                api_key: None,
                context_windows: BTreeMap::new(),
                max_output_tokens: BTreeMap::new(),
                chat_output_token_field: Default::default(),
                supports_vision: [("test-model".to_string(), false)].into_iter().collect(),
                reasoning_efforts: Default::default(),
                disabled_models: Default::default(),
                model_configs: Default::default(),
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
            template_id: None,
            base_url: "https://api.example.com/v1".to_string(),
            models: vec!["test-model".to_string()],
            api_backend: "responses".to_string(),
            api_key: None,
            context_windows: BTreeMap::new(),
            max_output_tokens: BTreeMap::new(),
            chat_output_token_field: Default::default(),
            supports_vision: [("test-model".to_string(), true)].into_iter().collect(),
            reasoning_efforts: Default::default(),
            disabled_models: Default::default(),
            model_configs: Default::default(),
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

    /// 构造一个用于导入测试的最小合法供应商记录。
    fn import_test_record(id: &str) -> ProviderRecord {
        ProviderRecord {
            id: id.to_string(),
            name: format!("Provider {id}"),
            template_id: None,
            base_url: "https://api.example.com/v1".to_string(),
            models: vec!["test-model".to_string()],
            api_backend: "responses".to_string(),
            api_key: None,
            context_windows: BTreeMap::new(),
            max_output_tokens: BTreeMap::new(),
            chat_output_token_field: Default::default(),
            supports_vision: [("test-model".to_string(), true)].into_iter().collect(),
            reasoning_efforts: Default::default(),
            disabled_models: Default::default(),
            model_configs: Default::default(),
        }
    }

    /// 导出文档结构必须能原样被导入解析器接受，且拒绝未知 schema 与版本。
    #[test]
    fn provider_import_parses_export_document() {
        let records = vec![
            import_test_record("provider-a"),
            import_test_record("provider-b"),
        ];
        let file = ProviderExportFile {
            schema: super::PROVIDER_EXPORT_SCHEMA.to_string(),
            version: super::PROVIDER_CONFIG_VERSION,
            providers: records,
        };
        let text = serde_json::to_string(&file).expect("序列化导出文档");
        let parsed = super::parse_provider_import(&text).expect("导出文档应可导入");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "provider-a");

        let mut unknown_schema = serde_json::to_value(&file).expect("导出文档转 JSON");
        unknown_schema["schema"] = "unknown/schema".into();
        assert!(super::parse_provider_import(unknown_schema.to_string().as_str()).is_err());

        let mut unknown_version = serde_json::to_value(&file).expect("导出文档转 JSON");
        unknown_version["version"] = 99.into();
        assert!(super::parse_provider_import(unknown_version.to_string().as_str()).is_err());
    }

    /// 导入同时接受完整配置文件 schema，并拒绝空列表和重复标识。
    #[test]
    fn provider_import_accepts_full_config_and_rejects_duplicates() {
        let state = ProviderState {
            active_provider_id: Some("provider-a".to_string()),
            active_model_id: Some("test-model".to_string()),
            revision: 0,
            providers: vec![import_test_record("provider-a")],
        };
        let file = ProviderFile::from_state(&state);
        let text = serde_json::to_string(&file).expect("序列化完整配置");
        let parsed = super::parse_provider_import(&text).expect("完整配置应可导入");
        assert_eq!(parsed.len(), 1);

        let empty = r#"{"schema":"keencode/providers-export","version":1,"providers":[]}"#;
        assert!(super::parse_provider_import(empty).is_err());

        let duplicated = r#"{"schema":"keencode/providers-export","version":1,"providers":[
            {"id":"a","name":"A","baseUrl":"https://api.example.com/v1","models":["m"],
             "apiBackend":"responses","apiKey":null,"contextWindows":{},
             "supportsVision":{"m":false}},
            {"id":"a","name":"A2","baseUrl":"https://api.example.com/v1","models":["m"],
             "apiBackend":"responses","apiKey":null,"contextWindows":{},
             "supportsVision":{"m":false}}]}"#;
        assert!(super::parse_provider_import(duplicated).is_err());
    }

    /// 合并语义：同标识覆盖、其余追加、不改变当前激活供应商与模型。
    #[test]
    fn provider_import_merges_by_id_without_touching_selection() {
        let mut existing = import_test_record("provider-a");
        existing.name = "Old Name".to_string();
        let state = ProviderState {
            active_provider_id: Some("provider-a".to_string()),
            active_model_id: Some("test-model".to_string()),
            revision: 0,
            providers: vec![existing],
        };
        let incoming = vec![
            import_test_record("provider-a"),
            import_test_record("provider-b"),
        ];
        let (merged, added, updated) =
            super::merge_provider_import(state, incoming).expect("合并导入记录");
        assert_eq!((added, updated), (1, 1));
        assert_eq!(merged.providers.len(), 2);
        assert_eq!(merged.providers[0].name, "Provider provider-a");
        assert_eq!(merged.active_provider_id.as_deref(), Some("provider-a"));
        assert_eq!(merged.active_model_id.as_deref(), Some("test-model"));
    }

    /// 空状态首次导入必须补齐当前供应商与模型，否则保存校验会整体失败。
    #[test]
    fn provider_import_into_empty_state_selects_first_provider() {
        let (merged, added, updated) = super::merge_provider_import(
            ProviderState::default(),
            vec![
                import_test_record("provider-a"),
                import_test_record("provider-b"),
            ],
        )
        .expect("空状态导入");
        assert_eq!((added, updated), (2, 0));
        assert_eq!(merged.active_provider_id.as_deref(), Some("provider-a"));
        assert_eq!(merged.active_model_id.as_deref(), Some("test-model"));
    }

    /// 覆盖当前供应商时删除了激活模型，必须回退到该供应商的首个模型。
    #[test]
    fn provider_import_falls_back_when_active_model_removed() {
        let mut removed_model = import_test_record("provider-a");
        removed_model.models = vec!["replacement-model".to_string()];
        removed_model.supports_vision = [("replacement-model".to_string(), false)]
            .into_iter()
            .collect();
        let state = ProviderState {
            active_provider_id: Some("provider-a".to_string()),
            active_model_id: Some("test-model".to_string()),
            revision: 0,
            providers: vec![import_test_record("provider-a")],
        };
        let (merged, added, updated) =
            super::merge_provider_import(state, vec![removed_model]).expect("覆盖当前供应商");
        assert_eq!((added, updated), (0, 1));
        assert_eq!(merged.active_provider_id.as_deref(), Some("provider-a"));
        assert_eq!(merged.active_model_id.as_deref(), Some("replacement-model"));
    }

    /// 合并结果必须通过完整状态校验；携带非法记录的导入整体失败。
    #[test]
    fn provider_import_merge_rejects_invalid_records() {
        let state = ProviderState::default();
        let mut record = import_test_record("provider-a");
        record.supports_vision.clear();
        assert!(super::merge_provider_import(state, vec![record]).is_err());
    }
}

#[cfg(test)]
mod provider_registry_tests {
    use super::{
        CustomProvider, FACADE_PLACEHOLDER_BASE_URL, MAX_PROVIDER_CONFIG_BYTES, ProviderRecord,
        ProviderState, ProvidersListResult, build_facade_model_selection_view,
        build_facade_settings_view, bump_revision, complete_model_config, facade_revision,
        load_state_from_path, parse_native_provider_timeout_override, provider_credential_revision,
        replace_runtime_registry, runtime_provider_config, save_state_to_path,
    };
    use keencode_model::{ModelProvider, ProviderProtocol};
    use keencode_provider::ProviderRegistry;
    use serde_json::Value;
    use std::collections::{BTreeMap, BTreeSet};
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
            max_output_tokens: BTreeMap::new(),
            chat_output_token_field: Default::default(),
            supports_vision: [(model.to_owned(), true)].into_iter().collect(),
            reasoning_efforts: Default::default(),
            disabled_models: Default::default(),
        }
    }

    #[test]
    fn disabled_models_are_excluded_from_runtime_registry() {
        let mut disabled = provider(
            "isolated",
            "https://models.example/v1/responses",
            "responses",
            None,
            "disabled-model",
        );
        disabled.models.push("enabled-model".to_owned());
        disabled
            .context_windows
            .insert("enabled-model".to_owned(), 64_000);
        disabled
            .supports_vision
            .insert("enabled-model".to_owned(), false);
        disabled.disabled_models.insert("disabled-model".to_owned());
        let registry = ProviderRegistry::new();
        replace_runtime_registry(
            &registry,
            &ProvidersListResult {
                providers: vec![disabled],
                default_model: Some("enabled-model".to_owned()),
                active_provider_id: Some("isolated".to_owned()),
            },
        )
        .expect("至少一个启用模型时应注册 Provider");
        assert!(registry.resolve("isolated", "disabled-model").is_err());
        assert!(registry.resolve("isolated", "enabled-model").is_ok());
    }

    #[test]
    fn facade_model_config_preserves_explicit_empty_reasoning_and_sparse_overlay() {
        let mut record = ProviderRecord {
            id: "facade".to_owned(),
            name: "Facade".to_owned(),
            template_id: None,
            base_url: "https://models.example/v1/responses".to_owned(),
            models: vec!["model".to_owned()],
            api_backend: "responses".to_owned(),
            api_key: None,
            context_windows: BTreeMap::new(),
            max_output_tokens: BTreeMap::new(),
            chat_output_token_field: Default::default(),
            supports_vision: [("model".to_owned(), false)].into_iter().collect(),
            reasoning_efforts: [("model".to_owned(), Vec::new())].into_iter().collect(),
            disabled_models: Default::default(),
            model_configs: Default::default(),
        };
        let inherited = complete_model_config(&record, "model", None, false);
        assert!(inherited["optionSpecs"].get("reasoningLevel").is_none());
        let empty_reasoning_overlay = serde_json::json!({
            "optionSpecs": {
                "reasoningLevel": {
                    "values": [],
                    "map": "{\"reasoning_effort\": reasoningLevel}"
                }
            }
        });
        let effective_without_reasoning =
            complete_model_config(&record, "model", Some(&empty_reasoning_overlay), false);
        assert!(
            effective_without_reasoning["optionSpecs"]
                .get("reasoningLevel")
                .is_none()
        );
        let overlay = serde_json::json!({
            "properties": {"inputFormat": {"supportsImage": true}},
            "optionSpecs": {"maxOutputTokens": {"max": 4096}}
        });
        record
            .model_configs
            .insert("model".to_owned(), overlay.clone());
        let effective = complete_model_config(&record, "model", Some(&overlay), false);
        assert_eq!(
            effective["properties"]["inputFormat"]["supportsImage"],
            true
        );
        assert_eq!(effective["optionSpecs"]["maxOutputTokens"]["max"], 4096);

        record
            .model_configs
            .insert("model".to_owned(), empty_reasoning_overlay);
        let settings = build_facade_settings_view(&ProviderState {
            active_provider_id: None,
            active_model_id: None,
            revision: 0,
            providers: vec![record],
        });
        assert!(
            settings["providers"][0]["models"][0]["personalExactConfig"]["optionSpecs"]
                .get("reasoningLevel")
                .is_none()
        );
    }

    #[test]
    fn facade_revision_is_persistent_and_monotonic() {
        let mut state = ProviderState::default();
        assert_eq!(facade_revision(&state), 0);
        bump_revision(&mut state);
        assert_eq!(facade_revision(&state), 1);
        bump_revision(&mut state);
        assert_eq!(facade_revision(&state), 2);
    }

    #[test]
    fn facade_selection_matches_source_identity_and_reasoning_fallback() {
        let state = ProviderState {
            active_provider_id: Some("facade".to_owned()),
            active_model_id: Some("model".to_owned()),
            revision: 0,
            providers: vec![ProviderRecord {
                id: "facade".to_owned(),
                name: "Facade".to_owned(),
                template_id: Some("custom-template".to_owned()),
                base_url: "https://models.example/v1/responses".to_owned(),
                models: vec!["model".to_owned()],
                api_backend: "responses".to_owned(),
                api_key: None,
                context_windows: BTreeMap::new(),
                max_output_tokens: BTreeMap::new(),
                chat_output_token_field: Default::default(),
                supports_vision: BTreeMap::new(),
                reasoning_efforts: Default::default(),
                disabled_models: Default::default(),
                model_configs: Default::default(),
            }],
        };
        let settings = build_facade_settings_view(&state);
        assert_eq!(settings["providers"][0]["templateId"], "custom-template");
        let view = build_facade_model_selection_view(&state, None);
        assert_eq!(view["providers"][0]["templateId"], "custom-template");
        assert_eq!(
            view["preferredSelection"]["options"]["reasoningLevel"],
            "max"
        );

        let missing_reasoning = build_facade_model_selection_view(
            &state,
            Some(&serde_json::json!({
                "selection": {"providerId": "facade", "modelId": "model"}
            })),
        );
        assert_eq!(
            missing_reasoning["effectiveSelection"],
            serde_json::json!({"providerId": "facade", "modelId": "model"})
        );
        assert_eq!(
            missing_reasoning["selectionIssue"],
            "reasoning-level-missing"
        );

        let mut empty_reasoning_state = state.clone();
        empty_reasoning_state
            .providers
            .first_mut()
            .expect("fixture provider")
            .reasoning_efforts
            .insert("model".to_owned(), Vec::new());
        let empty_reasoning_view = build_facade_model_selection_view(&empty_reasoning_state, None);
        assert_eq!(
            empty_reasoning_view["preferredSelection"],
            serde_json::json!({
                "providerId": "facade",
                "modelId": "model",
                "options": {}
            })
        );

        let unknown_model = build_facade_model_selection_view(
            &state,
            Some(&serde_json::json!({
                "selection": {
                    "providerId": "facade",
                    "modelId": "unknown",
                    "options": {"reasoningLevel": "max"}
                }
            })),
        );
        assert_eq!(unknown_model["effectiveSelection"], serde_json::Value::Null);
        assert_eq!(unknown_model["selectionIssue"], "model-not-found");
    }

    #[test]
    fn facade_empty_provider_state_has_no_runtime_selection() {
        let state = ProviderState {
            active_provider_id: None,
            active_model_id: None,
            revision: 0,
            providers: vec![ProviderRecord {
                id: "personal-provider-1".to_owned(),
                name: "New Provider".to_owned(),
                template_id: None,
                base_url: FACADE_PLACEHOLDER_BASE_URL.to_owned(),
                models: Vec::new(),
                api_backend: "responses".to_owned(),
                api_key: None,
                context_windows: BTreeMap::new(),
                max_output_tokens: BTreeMap::new(),
                chat_output_token_field: Default::default(),
                supports_vision: BTreeMap::new(),
                reasoning_efforts: BTreeMap::new(),
                disabled_models: BTreeSet::new(),
                model_configs: BTreeMap::new(),
            }],
        };
        assert!(super::validate_state(&state).is_ok());
        let view = build_facade_model_selection_view(&state, None);
        assert_eq!(view["providers"], serde_json::json!([]));
        assert!(view.get("preferredSelection").is_none());
    }

    /// 由 Rust facade builder 生成给 workflow/native-live 使用的脱敏契约夹具。
    ///
    /// 测试只打印结构化结果；实际文件由测试输出提取，避免手写一份可能漂移的 JSON。
    #[test]
    fn provider_facade_contract_fixture_is_rust_generated() {
        let state = ProviderState {
            active_provider_id: Some("fixture-provider".to_owned()),
            active_model_id: Some("fixture-model".to_owned()),
            revision: 7,
            providers: vec![ProviderRecord {
                id: "fixture-provider".to_owned(),
                name: "Fixture Provider".to_owned(),
                template_id: Some("custom-fixture".to_owned()),
                base_url: "https://example.invalid/v1/responses".to_owned(),
                models: vec!["fixture-model".to_owned(), "disabled-model".to_owned()],
                api_backend: "responses".to_owned(),
                api_key: None,
                context_windows: [("fixture-model".to_owned(), 128_000)]
                    .into_iter()
                    .collect(),
                max_output_tokens: BTreeMap::new(),
                chat_output_token_field: Default::default(),
                supports_vision: [("fixture-model".to_owned(), true)].into_iter().collect(),
                reasoning_efforts: [("fixture-model".to_owned(), Vec::new())]
                    .into_iter()
                    .collect(),
                disabled_models: ["disabled-model".to_owned()].into_iter().collect(),
                model_configs: [(
                    "fixture-model".to_owned(),
                    serde_json::json!({
                        "properties": {"inputFormat": {"supportsImage": true}},
                        "optionSpecs": {"maxOutputTokens": {"max": 8192}}
                    }),
                )]
                .into_iter()
                .collect(),
            }],
        };
        let settings = build_facade_settings_view(&state);
        let mut event = settings.clone();
        event["providers"][0]["effectiveConfig"]["access"]["apiKey"] =
            Value::String("fixture-secret-to-remove".to_owned());
        crate::frontend_rpc::providers::scrub_provider_credentials(&mut event);
        assert!(
            !serde_json::to_string(&event)
                .expect("序列化脱敏事件")
                .contains("fixture-secret-to-remove")
        );
        let selection = build_facade_model_selection_view(&state, None);
        // 明确关闭推理档位不会取消默认模型，也不会把合法的无档位选择标为缺失。
        let expected_selection = serde_json::json!({
            "providerId": "fixture-provider", "modelId": "fixture-model", "options": {}
        });
        assert_eq!(selection["preferredSelection"], expected_selection);
        let explicit = build_facade_model_selection_view(
            &state,
            Some(&serde_json::json!({"selection": expected_selection})),
        );
        assert_eq!(explicit["effectiveSelection"], expected_selection);
        assert!(explicit.get("selectionIssue").is_none());
        let fixture = serde_json::json!({
            "settingsGetView": settings,
            "settingsEvent": event,
            "modelSelectionGetView": selection,
        });
        println!(
            "PROVIDER_FACADE_CONTRACT_FIXTURE={}",
            serde_json::to_string(&fixture).expect("序列化 Provider facade 夹具")
        );
    }

    #[test]
    fn native_provider_timeout_guard_requires_isolated_native_context() {
        assert_eq!(
            parse_native_provider_timeout_override(false, true, true, Some("1")).unwrap(),
            None
        );
        assert_eq!(
            parse_native_provider_timeout_override(true, false, true, Some("1")).unwrap(),
            None
        );
        assert_eq!(
            parse_native_provider_timeout_override(true, true, false, Some("1")).unwrap(),
            None
        );
        // 普通构建即使继承了非法环境值，也不能改变或阻断生产配置。
        assert_eq!(
            parse_native_provider_timeout_override(false, false, false, Some("invalid")).unwrap(),
            None
        );
        assert_eq!(
            parse_native_provider_timeout_override(true, true, true, None).unwrap(),
            None
        );
    }

    #[test]
    fn native_provider_timeout_override_accepts_inclusive_boundaries() {
        assert_eq!(
            parse_native_provider_timeout_override(true, true, true, Some("1")).unwrap(),
            Some(std::time::Duration::from_millis(1))
        );
        assert_eq!(
            parse_native_provider_timeout_override(true, true, true, Some("300000")).unwrap(),
            Some(std::time::Duration::from_millis(300_000))
        );
    }

    #[test]
    fn native_provider_timeout_override_rejects_invalid_values() {
        for raw in ["", "0", "300001", "1.5", "-1", "not-a-number"] {
            assert!(
                parse_native_provider_timeout_override(true, true, true, Some(raw)).is_err(),
                "非法原生请求超时必须显式失败: {raw}"
            );
        }
    }

    #[test]
    fn output_budget_uses_configured_value_and_default() {
        let mut value = provider(
            "output",
            "https://example.invalid",
            "chat_completions",
            None,
            "model",
        );
        assert_eq!(
            runtime_provider_config(&value)
                .unwrap()
                .capabilities_for("model")
                .max_output_tokens,
            Some(128_000)
        );
        value.max_output_tokens.insert("model".to_owned(), 64_000);
        assert_eq!(
            runtime_provider_config(&value)
                .unwrap()
                .capabilities_for("model")
                .max_output_tokens,
            Some(64_000)
        );
        assert!(
            super::validate_max_output_tokens([("model".to_owned(), 0)].into(), &value.models)
                .is_err()
        );
        assert!(
            super::validate_max_output_tokens(
                [("unknown".to_owned(), 128_000)].into(),
                &value.models
            )
            .is_err()
        );
        let mut record: super::ProviderRecord =
            serde_json::from_value(serde_json::to_value(&value).unwrap()).unwrap();
        assert_eq!(record.max_output_tokens["model"], 64_000);
        record.max_output_tokens.insert("model".to_owned(), 96_000);
        let restored: super::ProviderRecord =
            serde_json::from_slice(&serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(restored.max_output_tokens["model"], 96_000);
    }

    /// 配置保存、读取与 Runtime 实际请求策略采用相同字段。
    #[test]
    fn gateway_policy_survives_storage_and_runtime_mapping() {
        let mut value = provider(
            "gateway",
            "http://127.0.0.1:1/v1",
            "chat_completions",
            None,
            "model",
        );
        value.chat_output_token_field = keencode_provider::ChatOutputTokenField::MaxTokens;
        let record: super::ProviderRecord =
            serde_json::from_value(serde_json::to_value(&value).unwrap()).unwrap();
        let loaded = super::render_list(ProviderState {
            active_provider_id: Some("gateway".into()),
            active_model_id: Some("model".into()),
            revision: 0,
            providers: vec![record],
        });
        let config = runtime_provider_config(&loaded.providers[0]).unwrap();
        assert_eq!(
            config.chat_output_token_field,
            keencode_provider::ChatOutputTokenField::MaxTokens
        );
        assert_eq!(config.read_timeout, std::time::Duration::from_secs(300));
        assert!(config.request_timeout.is_none());
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

    /// Anthropic Messages 后端自动启用提示缓存能力，其他协议维持关闭。
    ///
    /// cache_control 断点语义只在 Messages 协议上有效，能力快照据此自动装配；
    /// 默认快照与已登记模型的 per-model 快照取值一致。
    #[test]
    fn prompt_caching_capability_follows_messages_protocol() {
        for (backend, endpoint, expected) in [
            ("messages", "messages", true),
            ("chat_completions", "chat/completions", false),
            ("responses", "responses", false),
        ] {
            let provider = provider(
                backend,
                &format!("https://models.example/v2/{endpoint}#"),
                backend,
                Some("test-key"),
                "test-model",
            );
            let config = runtime_provider_config(&provider).expect("协议配置应映射");
            assert_eq!(
                config.default_capabilities.prompt_caching, expected,
                "{backend} 默认能力快照"
            );
            assert_eq!(
                config.capabilities_for("test-model").prompt_caching,
                expected,
                "{backend} 模型能力快照"
            );
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

    /// 注册表能力按手工窗口生成；未配置窗口回退 200K，且始终保留基础流式与工具能力。
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
            "configured-model".to_owned(),
            "default-model".to_owned(),
        ];
        provider
            .context_windows
            .insert("manual-model".to_owned(), 128_000);
        provider
            .context_windows
            .insert("configured-model".to_owned(), 256_000);

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

        let configured = registry
            .resolve("gateway", "configured-model")
            .expect("手工窗口模型应解析")
            .capabilities("configured-model");
        assert_eq!(configured.max_context_tokens, Some(256_000));

        let default = registry
            .resolve("gateway", "default-model")
            .expect("未配置窗口模型应解析")
            .capabilities("default-model");
        assert_eq!(
            default.max_context_tokens,
            Some(super::DEFAULT_CONTEXT_WINDOW_TOKENS)
        );
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
    fn replacement_preserves_inflight_resolution_and_activates_new_snapshot() {
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
        let old_capabilities = old_resolution.capabilities("old-model");
        assert!(old_capabilities.streaming);

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
        assert_eq!(old_resolution.capabilities("old-model"), old_capabilities);
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

    /// 损坏与非当前版本必须失败关闭，且不得覆盖原配置字节。
    #[test]
    fn invalid_provider_config_is_rejected_without_replacement() {
        let directory = tempfile::tempdir().expect("创建供应商配置临时目录");
        let path = directory.path().join("providers.json");
        let cases = [
            b"not-json".as_slice(),
            br#"{"schema":"keencode/providers","version":0,"activeProviderId":null,"activeModelId":null,"providers":[]}"#,
            br#"{"schema":"other/schema","version":1,"activeProviderId":null,"activeModelId":null,"providers":[]}"#,
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

    /// 含已移除字段的旧配置必须可加载：这是版本演进后的常规启动路径。
    #[test]
    fn provider_config_with_removed_fields_loads_and_is_usable() {
        let directory = tempfile::tempdir().expect("创建供应商配置临时目录");
        let path = directory.path().join("providers.json");
        let original = br#"{"schema":"keencode/providers","version":1,
            "activeProviderId":"provider","activeModelId":"test-model",
            "removedTopLevel":1,
            "providers":[{"id":"provider","name":"Provider",
            "baseUrl":"https://api.example.com/v1","models":["test-model"],
            "apiBackend":"responses","apiKey":"secret",
            "contextWindows":{},"removedProviderField":true,
            "supportsVision":{"test-model":false}}]}"#;
        fs::write(&path, original).expect("写入含已移除字段的供应商配置");

        let state = load_state_from_path(&path).expect("已移除字段不应阻断加载");
        assert_eq!(state.providers.len(), 1);
        assert_eq!(state.providers[0].api_key.as_deref(), Some("secret"));
        assert_eq!(state.active_model_id.as_deref(), Some("test-model"));
        assert_eq!(fs::read(&path).unwrap(), original, "加载不得改写原文件");
    }

    /// 事发形态回归：携带已移除 `context1m` 的多供应商配置必须完整可加载，
    /// 且加载产物能通过 Runtime 注册映射——这是发送消息链路的前置条件。
    #[test]
    fn provider_config_with_removed_context1m_maps_to_runtime_registry() {
        let directory = tempfile::tempdir().expect("创建供应商配置临时目录");
        let path = directory.path().join("providers.json");
        let original = br#"{"schema":"keencode/providers","version":1,
            "activeProviderId":"zcode","activeModelId":"glm-5.3-flash",
            "providers":[
              {"id":"zcode","name":"ZCode","baseUrl":"https://api.example.com/v1",
               "models":["glm-5.3-flash"],"apiBackend":"chat_completions",
               "apiKey":null,"contextWindows":{},
               "maxOutputTokens":{"glm-5.3-flash":131000},
               "chatOutputTokenField":"max_completion_tokens",
               "readTimeoutSeconds":500,
               "context1m":{},
               "supportsVision":{"glm-5.3-flash":false}},
              {"id":"router","name":"OpenRouter","baseUrl":"https://api.example.org/v1",
               "models":["union-alpha"],"apiBackend":"responses",
               "apiKey":"router-key","contextWindows":{"union-alpha":262144},
               "chatOutputTokenField":"max_tokens",
               "context1m":{"union-alpha":true},
               "supportsVision":{"union-alpha":true}}]}"#;
        fs::write(&path, original).expect("写入事发形态配置");

        let state = load_state_from_path(&path).expect("context1m 不应阻断加载");
        assert_eq!(state.providers.len(), 2);
        assert_eq!(state.active_model_id.as_deref(), Some("glm-5.3-flash"));

        // 加载产物必须能走完 Runtime 注册，验证发送链路真正恢复。
        let list = super::render_list(state);
        let registry = ProviderRegistry::new();
        super::replace_runtime_registry(&registry, &list)
            .expect("宽容加载的配置应能注册到 Runtime");
        assert!(
            registry.resolve("zcode", "glm-5.3-flash").is_ok(),
            "当前激活模型必须可解析"
        );
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
