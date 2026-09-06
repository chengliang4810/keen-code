use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, RwLock};

use keencode_model::{
    ModelError, ModelFuture, ModelProvider, ModelRequest, ModelStream, ProviderCapabilities,
    ProviderProtocol,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::{is_dangerous_identifier_character, validate_provider_id};
use crate::{ProviderClient, ProviderConfig, ProviderConfigError, RequestObserver};

/// Provider 显示名称允许的最大 UTF-8 字节数。
const MAX_DISPLAY_NAME_BYTES: usize = 256;
/// 单个模型标识允许的最大 UTF-8 字节数。
const MAX_MODEL_ID_BYTES: usize = 1024;
/// 凭据修订标识允许的最大 ASCII 字节数。
const MAX_CREDENTIAL_REVISION_BYTES: usize = 128;
/// 配置身份摘要使用的独立哈希域。
const CONFIG_IDENTITY_DOMAIN: &[u8] = b"keencode-provider-config-identity-v1";

/// Provider 对模型标识采用的显式接受策略。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderModelPolicy {
    /// 只允许非空精确模型集合中的成员。
    Enumerated {
        /// 按配置或模型目录顺序保存的非空精确模型集合。
        models: Vec<String>,
    },
    /// 明确允许任意通过安全边界校验的非空模型标识。
    AllowAny,
}

/// 一次原子注册所需的 Provider 配置、显示名称、凭据修订和模型策略。
pub struct ProviderRegistration {
    /// 将被构造成不可变客户端的完整 Provider 配置。
    config: ProviderConfig,
    /// 设置界面和列表使用的非空显示名称。
    display_name: String,
    /// 由配置存储独立维护且在凭据变化时更新的非秘密稳定修订标识。
    credential_revision: String,
    /// 已完成非空和重复成员校验的显式模型策略。
    model_policy: RegisteredModelPolicy,
}

impl ProviderRegistration {
    /// 创建一个经过显示名称、凭据修订和模型策略校验的 Provider 注册项。
    pub fn new(
        config: ProviderConfig,
        display_name: impl Into<String>,
        credential_revision: impl Into<String>,
        model_policy: ProviderModelPolicy,
    ) -> Result<Self, ProviderRegistryError> {
        config.validate().map_err(ProviderRegistryError::Config)?;
        let display_name = display_name.into();
        validate_display_name(&display_name)?;
        let credential_revision = credential_revision.into();
        validate_credential_revision(&credential_revision)?;
        let model_policy = validate_model_policy(model_policy)?;
        Ok(Self {
            config,
            display_name,
            credential_revision,
            model_policy,
        })
    }
}

/// 不含凭据或 HTTP 客户端的 Provider 注册表列表项。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRegistrationSummary {
    /// 用户配置中的稳定 Provider 标识。
    pub provider_id: String,
    /// 设置界面使用的 Provider 显示名称。
    pub display_name: String,
    /// 此客户端实例固定使用的厂商协议。
    pub protocol: ProviderProtocol,
    /// 当前注册项明确采用的模型接受策略。
    pub model_policy: ProviderModelPolicy,
    /// 不含凭据身份的稳定传输配置摘要。
    pub transport_fingerprint: String,
    /// 同时绑定传输配置和外部凭据修订、但不绑定凭据正文的稳定配置身份。
    pub config_identity: String,
}

/// 一次原子注册表状态的只读投影。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRegistrySnapshot {
    /// 每次成功整体替换后严格递增的进程内代次。
    pub generation: u64,
    /// 按 Provider 标识排序的完整非敏感注册项。
    pub providers: Vec<ProviderRegistrationSummary>,
}

/// 已绑定 Provider、模型、协议和配置身份的一次可失效解析结果。
#[derive(Clone)]
pub struct ResolvedProvider {
    /// 解析发生时的完整注册表代次。
    generation: u64,
    /// 用户配置中的稳定 Provider 标识。
    provider_id: String,
    /// 将写入请求和 Session 快照的精确模型标识。
    model: String,
    /// 客户端实例固定使用的厂商协议。
    protocol: ProviderProtocol,
    /// 不含凭据身份的稳定传输配置摘要。
    transport_fingerprint: String,
    /// 同时绑定传输配置与凭据修订的稳定配置身份。
    config_identity: String,
    /// 用于每次新请求前校验代次并取得当前客户端的共享注册表状态。
    registry_state: Arc<RwLock<Arc<RegistryState>>>,
}

impl ResolvedProvider {
    /// 返回解析发生时的完整注册表代次。
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// 返回稳定 Provider 标识。
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    /// 返回严格绑定的精确模型标识。
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 返回实际调用使用的厂商协议。
    pub const fn protocol(&self) -> ProviderProtocol {
        self.protocol
    }

    /// 返回不含凭据身份的稳定传输配置摘要。
    pub fn transport_fingerprint(&self) -> &str {
        &self.transport_fingerprint
    }

    /// 返回同时绑定传输配置和凭据修订的稳定配置身份。
    pub fn config_identity(&self) -> &str {
        &self.config_identity
    }
}

impl ModelProvider for ResolvedProvider {
    /// 只为当前代次中严格绑定的模型返回能力快照。
    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        if model != self.model {
            return ProviderCapabilities::default();
        }
        let Ok(state) = self.registry_state.read() else {
            return ProviderCapabilities::default();
        };
        if state.generation != self.generation {
            return ProviderCapabilities::default();
        }
        state
            .providers
            .get(&self.provider_id)
            .map_or_else(ProviderCapabilities::default, |provider| {
                provider.client.capabilities(model)
            })
    }

    /// 在 Future 首次执行时校验模型和代次，再把已经开始的请求交给不可变客户端完成。
    fn stream(&self, request: ModelRequest) -> ModelFuture<'_, Result<ModelStream, ModelError>> {
        let registry_state = self.registry_state.clone();
        let generation = self.generation;
        let provider_id = self.provider_id.clone();
        let model = self.model.clone();
        Box::pin(async move {
            if request.model != model {
                return Err(bound_model_mismatch());
            }
            let client = {
                let state = registry_state
                    .read()
                    .map_err(|_| stale_provider_resolution())?;
                if state.generation != generation {
                    return Err(stale_provider_resolution());
                }
                state
                    .providers
                    .get(&provider_id)
                    .map(|provider| provider.client.clone())
                    .ok_or_else(stale_provider_resolution)?
            };
            client.stream(request).await
        })
    }
}

impl fmt::Debug for ResolvedProvider {
    /// 只输出不含客户端或凭据的解析元数据。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedProvider")
            .field("generation", &self.generation)
            .field("provider_id", &self.provider_id)
            .field("model", &self.model)
            .field("protocol", &self.protocol)
            .field("transport_fingerprint", &self.transport_fingerprint)
            .field("config_identity", &self.config_identity)
            .finish_non_exhaustive()
    }
}

/// 可并发读取并按完整快照原子热替换的 Provider 注册表。
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    /// 当前不可变注册表状态；共享锁只覆盖代次检查或单次指针级整体替换。
    state: Arc<RwLock<Arc<RegistryState>>>,
    /// 每个新注册 Provider Client 共享的可选请求观测器。
    observer: Option<Arc<dyn RequestObserver>>,
}

/// 一个不可部分可见的注册表代次。
#[derive(Default)]
struct RegistryState {
    /// 当前代次，空注册表为零。
    generation: u64,
    /// 按稳定 Provider 标识保存的不可变客户端。
    providers: BTreeMap<String, RegisteredProvider>,
}

/// 已校验且不会把空枚举误解释为任意模型的内部模型策略。
enum RegisteredModelPolicy {
    /// 只允许精确集合成员，同时保留用户给定顺序。
    Enumerated {
        /// 用于常数对数级成员查询的精确集合。
        models: BTreeSet<String>,
        /// 用于列表投影的原始稳定顺序。
        ordered_models: Vec<String>,
    },
    /// 调用方明确选择了任意有效模型策略。
    AllowAny,
}

impl RegisteredModelPolicy {
    /// 判断一个已经通过安全边界校验的模型是否满足当前显式策略。
    fn allows(&self, model: &str) -> bool {
        match self {
            Self::Enumerated { models, .. } => models.contains(model),
            Self::AllowAny => true,
        }
    }

    /// 构造不会丢失“枚举”与“任意”差异的非敏感列表投影。
    fn snapshot(&self) -> ProviderModelPolicy {
        match self {
            Self::Enumerated { ordered_models, .. } => ProviderModelPolicy::Enumerated {
                models: ordered_models.clone(),
            },
            Self::AllowAny => ProviderModelPolicy::AllowAny,
        }
    }
}

/// 注册表内部同时持有调用客户端与非敏感显示元数据的条目。
struct RegisteredProvider {
    /// 设置界面使用的显示名称。
    display_name: String,
    /// 明确区分非空精确集合与任意模型的选择策略。
    model_policy: RegisteredModelPolicy,
    /// 不含凭据身份的传输配置摘要。
    transport_fingerprint: String,
    /// 同时绑定传输配置和凭据修订的配置身份。
    config_identity: String,
    /// 当前代次内共享连接池的不可变客户端。
    client: Arc<ProviderClient>,
}

impl ProviderRegistry {
    /// 创建代次为零且不包含 Provider 的空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 创建会把全部真实模型请求短元数据交给同一观察者的注册表。
    pub fn with_request_observer(observer: Arc<dyn RequestObserver>) -> Self {
        Self {
            state: Arc::new(RwLock::new(Arc::new(RegistryState::default()))),
            observer: Some(observer),
        }
    }

    /// 先完整构造新客户端，再以一次写锁整体替换注册表并返回新快照。
    pub fn replace_all(
        &self,
        registrations: impl IntoIterator<Item = ProviderRegistration>,
    ) -> Result<ProviderRegistrySnapshot, ProviderRegistryError> {
        let mut providers = BTreeMap::new();
        for registration in registrations {
            let provider_id = registration.config.id.clone();
            if providers.contains_key(&provider_id) {
                return Err(ProviderRegistryError::DuplicateProvider);
            }
            let transport_fingerprint = registration
                .config
                .transport_fingerprint()
                .map_err(ProviderRegistryError::Config)?;
            let config_identity =
                config_identity(&transport_fingerprint, &registration.credential_revision);
            let client =
                ProviderClient::new(registration.config).map_err(ProviderRegistryError::Config)?;
            let client = match &self.observer {
                Some(observer) => client.with_request_observer(Arc::clone(observer)),
                None => client,
            };
            let client = Arc::new(client);
            providers.insert(
                provider_id,
                RegisteredProvider {
                    display_name: registration.display_name,
                    model_policy: registration.model_policy,
                    transport_fingerprint,
                    config_identity,
                    client,
                },
            );
        }

        let mut state = self
            .state
            .write()
            .map_err(|_| ProviderRegistryError::Unavailable)?;
        let generation = state
            .generation
            .checked_add(1)
            .ok_or(ProviderRegistryError::GenerationExhausted)?;
        let next = Arc::new(RegistryState {
            generation,
            providers,
        });
        *state = next.clone();
        drop(state);
        Ok(snapshot_from_state(&next))
    }

    /// 按独立 Provider 与模型字段解析一次会在后续成功替换时失效的绑定客户端。
    pub fn resolve(
        &self,
        provider_id: &str,
        model: &str,
    ) -> Result<ResolvedProvider, ProviderRegistryError> {
        validate_provider_id(provider_id).map_err(|_| ProviderRegistryError::InvalidProviderId)?;
        validate_model_id(model)?;
        let state = self
            .state
            .read()
            .map_err(|_| ProviderRegistryError::Unavailable)?
            .clone();
        let provider = state
            .providers
            .get(provider_id)
            .ok_or(ProviderRegistryError::UnknownProvider)?;
        if !provider.model_policy.allows(model) {
            return Err(ProviderRegistryError::UnknownModel);
        }
        Ok(ResolvedProvider {
            generation: state.generation,
            provider_id: provider_id.to_owned(),
            model: model.to_owned(),
            protocol: provider.client.config().protocol,
            transport_fingerprint: provider.transport_fingerprint.clone(),
            config_identity: provider.config_identity.clone(),
            registry_state: self.state.clone(),
        })
    }

    /// 返回当前完整代次的非敏感只读投影。
    pub fn snapshot(&self) -> Result<ProviderRegistrySnapshot, ProviderRegistryError> {
        let state = self
            .state
            .read()
            .map_err(|_| ProviderRegistryError::Unavailable)?
            .clone();
        Ok(snapshot_from_state(&state))
    }
}

/// 从不可变注册表状态构造不含凭据的列表投影。
fn snapshot_from_state(state: &RegistryState) -> ProviderRegistrySnapshot {
    ProviderRegistrySnapshot {
        generation: state.generation,
        providers: state
            .providers
            .iter()
            .map(|(provider_id, provider)| ProviderRegistrationSummary {
                provider_id: provider_id.clone(),
                display_name: provider.display_name.clone(),
                protocol: provider.client.config().protocol,
                model_policy: provider.model_policy.snapshot(),
                transport_fingerprint: provider.transport_fingerprint.clone(),
                config_identity: provider.config_identity.clone(),
            })
            .collect(),
    }
}

/// 校验并转换显式模型策略，禁止空枚举静默退化成任意模型。
fn validate_model_policy(
    model_policy: ProviderModelPolicy,
) -> Result<RegisteredModelPolicy, ProviderRegistryError> {
    match model_policy {
        ProviderModelPolicy::Enumerated { models } => {
            if models.is_empty() {
                return Err(ProviderRegistryError::EmptyEnumeratedModels);
            }
            let mut unique = BTreeSet::new();
            for model in &models {
                validate_model_id(model)?;
                if !unique.insert(model.clone()) {
                    return Err(ProviderRegistryError::DuplicateModel);
                }
            }
            Ok(RegisteredModelPolicy::Enumerated {
                models: unique,
                ordered_models: models,
            })
        }
        ProviderModelPolicy::AllowAny => Ok(RegisteredModelPolicy::AllowAny),
    }
}

/// 校验 Provider 显示名称适合进入本地列表与诊断输出。
fn validate_display_name(display_name: &str) -> Result<(), ProviderRegistryError> {
    if display_name.trim().is_empty()
        || display_name.trim() != display_name
        || display_name.len() > MAX_DISPLAY_NAME_BYTES
        || display_name.chars().any(is_dangerous_identifier_character)
    {
        return Err(ProviderRegistryError::InvalidDisplayName);
    }
    Ok(())
}

/// 校验模型标识具有统一长度边界且不会产生危险显示控制效果。
fn validate_model_id(model: &str) -> Result<(), ProviderRegistryError> {
    if model.trim().is_empty()
        || model.trim() != model
        || model.len() > MAX_MODEL_ID_BYTES
        || model.chars().any(is_dangerous_identifier_character)
    {
        return Err(ProviderRegistryError::InvalidModel);
    }
    Ok(())
}

/// 校验凭据修订是非空、有限且只包含安全 ASCII 字符的配置存储标识。
fn validate_credential_revision(revision: &str) -> Result<(), ProviderRegistryError> {
    if revision.is_empty()
        || revision.len() > MAX_CREDENTIAL_REVISION_BYTES
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ProviderRegistryError::InvalidCredentialRevision);
    }
    Ok(())
}

/// 计算只绑定传输摘要和外部凭据修订、绝不读取或散列凭据正文的配置身份。
fn config_identity(transport_fingerprint: &str, credential_revision: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(CONFIG_IDENTITY_DOMAIN);
    for part in [
        transport_fingerprint.as_bytes(),
        credential_revision.as_bytes(),
    ] {
        let length = u64::try_from(part.len()).expect("已校验配置身份字段长度必须能表示为 u64");
        digest.update(length.to_be_bytes());
        digest.update(part);
    }
    let digest = digest.finalize();
    let mut output = String::with_capacity("sha256:".len() + digest.len() * 2);
    output.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

/// 构造不包含 Provider、模型或凭据内容的绑定模型不一致错误。
fn bound_model_mismatch() -> ModelError {
    ModelError::InvalidRequest {
        message: "模型请求与已解析的 Provider 模型绑定不一致".to_owned(),
    }
}

/// 构造要求调用方重新解析当前 Provider 的稳定失效错误。
fn stale_provider_resolution() -> ModelError {
    ModelError::InvalidRequest {
        message: "Provider 解析结果已失效，请按当前注册表重新解析".to_owned(),
    }
}

/// Provider 注册、热替换或结构化模型选择失败。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderRegistryError {
    /// 底层 Provider 配置或 HTTP 客户端无效。
    Config(ProviderConfigError),
    /// Provider 查询标识为空、过长、带边界空白或危险显示字符。
    InvalidProviderId,
    /// Provider 显示名称为空、带边界空白、危险显示字符或超过上限。
    InvalidDisplayName,
    /// 凭据修订为空、超过上限或包含非安全 ASCII 字符。
    InvalidCredentialRevision,
    /// 精确枚举策略没有提供任何模型，禁止静默放开任意模型。
    EmptyEnumeratedModels,
    /// 模型标识为空、带边界空白、危险显示字符或超过上限。
    InvalidModel,
    /// 同一次整体替换包含重复 Provider 标识。
    DuplicateProvider,
    /// 一个 Provider 注册项包含重复模型标识。
    DuplicateModel,
    /// 选择的 Provider 当前没有注册。
    UnknownProvider,
    /// Provider 的精确模型集合中不存在目标模型。
    UnknownModel,
    /// 注册表读写锁已经损坏，禁止继续解析或替换。
    Unavailable,
    /// 注册表代次达到 `u64::MAX`，禁止回绕并混淆会话快照。
    GenerationExhausted,
}

impl fmt::Display for ProviderRegistryError {
    /// 输出不会原样回显不可信 Provider、模型或凭据修订的稳定错误说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(formatter, "Provider 配置无效：{error}"),
            Self::InvalidProviderId => formatter.write_str("Provider 标识无效"),
            Self::InvalidDisplayName => formatter.write_str("Provider 显示名称无效"),
            Self::InvalidCredentialRevision => formatter.write_str("Provider 凭据修订无效"),
            Self::EmptyEnumeratedModels => formatter.write_str("Provider 精确模型集合不能为空"),
            Self::InvalidModel => formatter.write_str("模型标识无效"),
            Self::DuplicateProvider => formatter.write_str("Provider 标识重复"),
            Self::DuplicateModel => formatter.write_str("Provider 模型标识重复"),
            Self::UnknownProvider => formatter.write_str("Provider 尚未注册"),
            Self::UnknownModel => formatter.write_str("Provider 未登记目标模型"),
            Self::Unavailable => formatter.write_str("Provider 注册表当前不可用"),
            Self::GenerationExhausted => formatter.write_str("Provider 注册表代次已耗尽"),
        }
    }
}

impl Error for ProviderRegistryError {}
