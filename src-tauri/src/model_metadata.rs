//! 模型价格、上下文容量与推理能力的按需目录。
//!
//! KeenCode 只保存用户实际查询过的模型，不把任何完整远端目录打进安装包。
//! 查询不依赖自定义供应商名称，仅按模型标识依次访问固定数据源，并对每个
//! 字段采用第一个有效结果。

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

use crate::http_response::{HttpResponseReadError, read_http_response_limited};

/// 串行化模型元数据缓存的刷新与落盘，避免并发请求覆盖彼此结果。
static MODEL_METADATA_LOCK: Mutex<()> = Mutex::new(());

/// 当前唯一的模型元数据缓存结构版本。
const CACHE_VERSION: u32 = 2;
/// 已缓存记录的有效期；过期后按固定顺序重新查询远端目录。
const CACHE_TTL_SECONDS: u64 = 24 * 60 * 60;
/// 本地最多保存的模型数量，限制长期使用后的缓存体积。
const MAX_CACHED_MODELS: usize = 256;
/// 单个远端目录允许下载的最大字节数。
const MAX_CATALOG_BYTES: usize = 5 * 1024 * 1024;
/// 本地缓存文件允许读取的最大字节数。
const MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;
/// 远端目录连接超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// 单个远端目录完整请求超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
/// 当前唯一的本地模型元数据文件名。
const CACHE_FILE_NAME: &str = "model-metadata.json";

/// 可查询的远端模型目录。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CatalogSource {
    /// Vercel AI Gateway 公共模型目录。
    Vercel,
    /// OpenRouter 公共模型目录。
    OpenRouter,
}

/// 固定查询顺序；不得依据网络返回顺序或运行时状态重新排序。
const SOURCE_ORDER: [CatalogSource; 2] = [CatalogSource::Vercel, CatalogSource::OpenRouter];

impl CatalogSource {
    /// 返回写入缓存的稳定数据源标识。
    fn id(self) -> &'static str {
        match self {
            Self::Vercel => "vercel",
            Self::OpenRouter => "openrouter",
        }
    }

    /// 返回公开模型目录地址。
    fn endpoint(self) -> &'static str {
        match self {
            Self::Vercel => "https://ai-gateway.vercel.sh/v1/models",
            Self::OpenRouter => "https://openrouter.ai/api/v1/models",
        }
    }

    /// 将当前数据源响应转换为统一候选记录。
    fn parse(self, document: &Value, model_id: &str) -> Option<SourceCandidate> {
        match self {
            Self::Vercel => parse_vercel_candidate(document, model_id),
            Self::OpenRouter => parse_openrouter_candidate(document, model_id),
        }
    }
}

/// 每百万 token 的美元价格，用于后续估算而非账单结算。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelPrice {
    /// 每百万输入 token 的美元价格。
    pub input_per_million: f64,
    /// 每百万输出 token 的美元价格。
    pub output_per_million: f64,
    /// 每百万缓存读取 token 的美元价格。
    pub cache_read_per_million: Option<f64>,
    /// 每百万缓存写入 token 的美元价格。
    pub cache_write_per_million: Option<f64>,
}

/// 模型公开的推理控制形式。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "type")]
pub enum ModelReasoningControl {
    /// 离散推理强度列表。
    Effort {
        /// 按 KeenCode 固定强度顺序排列的可用值。
        values: Vec<String>,
    },
    /// 仅允许打开或关闭推理。
    Toggle,
    /// 使用推理 token 预算控制推理量。
    BudgetTokens {
        /// 最小推理 token 数；远端未声明时为空。
        min: Option<u64>,
        /// 最大推理 token 数；远端未声明时为空。
        max: Option<u64>,
    },
}

/// 模型推理能力及其可调参数。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelReasoningInfo {
    /// 当前目录是否明确声明支持推理。
    pub supported: bool,
    /// 当前目录声明的推理控制形式。
    pub controls: Vec<ModelReasoningControl>,
    /// 当前目录声明的默认推理强度。
    pub default_effort: Option<String>,
    /// 当前目录是否声明推理不可关闭。
    pub mandatory: Option<bool>,
}

/// 单个字段的来源与实际匹配模型。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelMetadataFieldSource {
    /// 远端目录稳定标识。
    pub catalog: String,
    /// 远端目录中实际命中的模型标识。
    pub matched_model_id: String,
}

/// 模型元数据各字段的来源。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelMetadataSources {
    /// 价格字段来源。
    pub price: Option<ModelMetadataFieldSource>,
    /// 上下文容量字段来源。
    pub context_window: Option<ModelMetadataFieldSource>,
    /// 最大输出 token 字段来源。
    pub max_output_tokens: Option<ModelMetadataFieldSource>,
    /// 推理信息字段来源。
    pub reasoning: Option<ModelMetadataFieldSource>,
    /// 图片输入能力字段来源。
    pub supports_vision: Option<ModelMetadataFieldSource>,
}

/// 前端与本地文件共享的单模型元数据。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelMetadata {
    /// 用户配置的原始模型标识，也是缓存查询键。
    pub model_id: String,
    /// 用于粗略费用统计的价格。
    pub price: Option<ModelPrice>,
    /// 模型上下文窗口 token 数。
    pub context_window: Option<u64>,
    /// 模型最大输出 token 数。
    pub max_output_tokens: Option<u64>,
    /// 模型推理支持与控制信息；为空表示未知。
    pub reasoning: Option<ModelReasoningInfo>,
    /// 是否支持图片输入；为空表示远端目录没有给出结论。
    pub supports_vision: Option<bool>,
    /// 每个字段采用的首个有效数据源。
    pub sources: ModelMetadataSources,
    /// 最近一次成功解析远端目录的 Unix 秒时间戳。
    pub updated_at: u64,
}

impl ModelMetadata {
    /// 创建尚未解析出任何字段的模型元数据。
    fn empty(model_id: &str, updated_at: u64) -> Self {
        Self {
            model_id: model_id.to_string(),
            price: None,
            context_window: None,
            max_output_tokens: None,
            reasoning: None,
            supports_vision: None,
            sources: ModelMetadataSources::default(),
            updated_at,
        }
    }

    /// 判断价格、上下文和推理三个核心字段是否都已获得。
    fn is_complete(&self) -> bool {
        self.price.is_some()
            && self.context_window.is_some()
            && self.reasoning.is_some()
            && self.supports_vision.is_some()
    }

    /// 仅用当前候选记录补齐仍为空的字段，绝不覆盖较早数据源的结果。
    fn merge_missing(&mut self, candidate: SourceCandidate) {
        let field_source = ModelMetadataFieldSource {
            catalog: candidate.source.id().to_string(),
            matched_model_id: candidate.matched_model_id,
        };
        if self.price.is_none()
            && let Some(price) = candidate.price
        {
            self.price = Some(price);
            self.sources.price = Some(field_source.clone());
        }
        if self.context_window.is_none()
            && let Some(context_window) = candidate.context_window
        {
            self.context_window = Some(context_window);
            self.sources.context_window = Some(field_source.clone());
        }
        if self.max_output_tokens.is_none()
            && let Some(max_output_tokens) = candidate.max_output_tokens
        {
            self.max_output_tokens = Some(max_output_tokens);
            self.sources.max_output_tokens = Some(field_source.clone());
        }
        if self.reasoning.is_none()
            && let Some(reasoning) = candidate.reasoning
        {
            self.reasoning = Some(reasoning);
            self.sources.reasoning = Some(field_source.clone());
        }
        if self.supports_vision.is_none()
            && let Some(supports_vision) = candidate.supports_vision
        {
            self.supports_vision = Some(supports_vision);
            self.sources.supports_vision = Some(field_source);
        }
    }
}

/// 当前唯一的本地模型元数据文件结构。
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ModelMetadataCache {
    /// 当前缓存结构版本。
    version: u32,
    /// 以用户原始模型标识索引的按需记录。
    models: BTreeMap<String, ModelMetadata>,
}

impl Default for ModelMetadataCache {
    /// 创建当前版本的空模型元数据缓存。
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            models: BTreeMap::new(),
        }
    }
}

/// 单个远端目录解析出的统一候选记录。
#[derive(Clone, Debug)]
struct SourceCandidate {
    /// 当前候选所属的远端目录。
    source: CatalogSource,
    /// 远端目录中实际命中的模型标识。
    matched_model_id: String,
    /// 当前候选提供的价格。
    price: Option<ModelPrice>,
    /// 当前候选提供的上下文窗口。
    context_window: Option<u64>,
    /// 当前候选提供的最大输出 token 数。
    max_output_tokens: Option<u64>,
    /// 当前候选提供的推理信息。
    reasoning: Option<ModelReasoningInfo>,
    /// 当前候选是否明确支持图片输入。
    supports_vision: Option<bool>,
}

/// Tauri 命令：按模型标识返回价格、上下文和推理元数据。
#[tauri::command]
pub async fn model_metadata_get(
    model_id: String,
    app: AppHandle,
) -> std::result::Result<ModelMetadata, String> {
    tauri::async_runtime::spawn_blocking(move || get(&app, &model_id))
        .await
        .map_err(|error| format!("模型元数据后台任务失败：{error}"))?
        .map_err(|error| error.to_string())
}

/// Tauri 命令：一次刷新多个模型，远端目录每个数据源最多下载一次。
#[tauri::command]
pub async fn model_metadata_get_many(
    model_ids: Vec<String>,
    app: AppHandle,
) -> std::result::Result<Vec<ModelMetadata>, String> {
    tauri::async_runtime::spawn_blocking(move || get_many(&app, &model_ids))
        .await
        .map_err(|error| format!("模型元数据后台任务失败：{error}"))?
        .map_err(|error| error.to_string())
}

/// 返回新鲜缓存，或按固定顺序刷新并保存单模型元数据。
fn get(app: &AppHandle, model_id: &str) -> Result<ModelMetadata> {
    get_many(app, &[model_id.to_string()])?
        .into_iter()
        .next()
        .context("模型元数据结果为空")
}

/// 返回多个模型的新鲜缓存；缺失项共享同一轮远端目录下载。
fn get_many(app: &AppHandle, raw_model_ids: &[String]) -> Result<Vec<ModelMetadata>> {
    if raw_model_ids.is_empty() || raw_model_ids.len() > MAX_CACHED_MODELS {
        anyhow::bail!("模型元数据批量查询数量必须为 1 到 {MAX_CACHED_MODELS}");
    }
    let model_ids = raw_model_ids
        .iter()
        .map(|model_id| validate_model_id(model_id))
        .collect::<Result<Vec<_>>>()?;
    let _guard = MODEL_METADATA_LOCK.lock().expect("模型元数据缓存锁已损坏");
    let path = cache_path(app)?;
    let mut cache = load_cache(&path)?;
    let now = unix_seconds();
    let pending = model_ids
        .iter()
        .filter(|model_id| {
            cache
                .models
                .get(*model_id)
                .is_none_or(|cached| now.saturating_sub(cached.updated_at) >= CACHE_TTL_SECONDS)
        })
        .cloned()
        .collect::<Vec<_>>();

    if !pending.is_empty() {
        let client = build_client()?;
        let mut documents = Vec::new();
        let mut failures = Vec::new();
        for source in SOURCE_ORDER {
            match fetch_document(&client, source) {
                Ok(document) => documents.push((source, document)),
                Err(error) => failures.push(format!("{}: {error:#}", source.id())),
            }
        }
        if documents.is_empty() {
            if pending
                .iter()
                .any(|model_id| !cache.models.contains_key(model_id))
            {
                anyhow::bail!("全部模型元数据目录请求失败：{}", failures.join("；"));
            }
        } else {
            for model_id in pending {
                let mut metadata = ModelMetadata::empty(&model_id, now);
                for (source, document) in &documents {
                    if metadata.is_complete() {
                        break;
                    }
                    if let Some(candidate) = source.parse(document, &model_id) {
                        metadata.merge_missing(candidate);
                    }
                }
                insert_bounded(&mut cache, metadata);
            }
            save_cache(&path, &cache)?;
        }
    }

    model_ids
        .iter()
        .map(|model_id| {
            cache
                .models
                .get(model_id)
                .cloned()
                .with_context(|| format!("模型元数据结果缺失：{model_id}"))
        })
        .collect()
}

/// 创建限制连接时间、完整请求时间与用户代理的目录客户端。
fn build_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent("KeenCode/0.0.1 model-metadata")
        .build()
        .context("创建模型元数据 HTTP 客户端失败")
}

/// 下载并校验单个公开目录的 JSON 文档。
fn fetch_document(client: &Client, source: CatalogSource) -> Result<Value> {
    let response = client
        .get(source.endpoint())
        .header("accept", "application/json")
        .send()
        .with_context(|| format!("请求 {} 模型目录失败", source.id()))?
        .error_for_status()
        .with_context(|| format!("{} 模型目录返回错误", source.id()))?;
    let bytes = match read_http_response_limited(response, MAX_CATALOG_BYTES) {
        Ok(bytes) => bytes,
        Err(HttpResponseReadError::TooLarge { .. }) => {
            anyhow::bail!("{} 模型目录超过大小限制", source.id());
        }
        Err(HttpResponseReadError::Read(error)) => {
            return Err(error).with_context(|| format!("读取 {} 模型目录失败", source.id()));
        }
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("解析 {} 模型目录 JSON 失败", source.id()))
}

/// 从 Vercel AI Gateway 响应中读取统一候选记录。
fn parse_vercel_candidate(document: &Value, model_id: &str) -> Option<SourceCandidate> {
    let row = find_model_row(model_rows(document), model_id)?;
    Some(SourceCandidate {
        source: CatalogSource::Vercel,
        matched_model_id: row.get("id")?.as_str()?.to_string(),
        price: parse_price(row.get("pricing"), "input", "output"),
        context_window: positive_u64(row.get("context_window")),
        max_output_tokens: positive_u64(row.get("max_tokens")),
        reasoning: parse_vercel_reasoning(row),
        supports_vision: input_modalities(row.get("modalities")),
    })
}

/// 从 OpenRouter 响应中读取统一候选记录。
fn parse_openrouter_candidate(document: &Value, model_id: &str) -> Option<SourceCandidate> {
    let row = find_model_row(model_rows(document), model_id)?;
    Some(SourceCandidate {
        source: CatalogSource::OpenRouter,
        matched_model_id: row.get("id")?.as_str()?.to_string(),
        price: parse_price(row.get("pricing"), "prompt", "completion"),
        context_window: positive_u64(row.get("context_length"))
            .or_else(|| positive_u64(row.get("top_provider")?.get("context_length"))),
        max_output_tokens: positive_u64(row.get("top_provider")?.get("max_completion_tokens")),
        reasoning: parse_openrouter_reasoning(row),
        supports_vision: input_modalities(row.get("architecture")),
    })
}

/// 目录明确返回输入模态时，以是否包含 image 判定视觉能力；字段缺失保持未知。
fn input_modalities(container: Option<&Value>) -> Option<bool> {
    let container = container?;
    let inputs = container
        .get("input")
        .or_else(|| container.get("input_modalities"))?;
    let inputs = inputs.as_array()?;
    Some(
        inputs
            .iter()
            .filter_map(Value::as_str)
            .any(|value| value == "image"),
    )
}

/// 返回目录文档中的模型数组；未知结构按空数组处理。
fn model_rows(document: &Value) -> &[Value] {
    document
        .get("data")
        .or_else(|| document.get("models"))
        .and_then(Value::as_array)
        .or_else(|| document.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// 按精确、尾段精确、尾段规范化三个等级稳定选择首个模型。
fn find_model_row<'a>(rows: &'a [Value], model_id: &str) -> Option<&'a Value> {
    rows.iter()
        .filter_map(|row| {
            let source_id = row.get("id")?.as_str()?;
            let rank = model_match_rank(source_id, model_id)?;
            Some((rank, source_id, row))
        })
        .min_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)))
        .map(|(_, _, row)| row)
}

/// 返回模型标识匹配等级；数值越小优先级越高。
fn model_match_rank(source_id: &str, model_id: &str) -> Option<u8> {
    if source_id == model_id {
        return Some(0);
    }
    let source_tail = source_id.rsplit('/').next()?;
    let query_tail = model_id.rsplit('/').next()?;
    if source_tail == query_tail {
        return Some(1);
    }
    if normalize_model_tail(source_tail) == normalize_model_tail(query_tail) {
        return Some(2);
    }
    None
}

/// 将模型尾段转为分隔符统一的比较键，兼容点号与短横线版本写法且保留版本边界。
fn normalize_model_tail(value: &str) -> String {
    let mut normalized = String::new();
    let mut separator_pending = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            if separator_pending && !normalized.is_empty() {
                normalized.push('-');
            }
            normalized.push(character.to_ascii_lowercase());
            separator_pending = false;
        } else {
            separator_pending = true;
        }
    }
    normalized
}

/// 读取输入、输出及可选缓存价格，并统一为每百万 token 美元。
fn parse_price(value: Option<&Value>, input_key: &str, output_key: &str) -> Option<ModelPrice> {
    let value = value?;
    let input_per_million = price_per_million(value.get(input_key)?)?;
    let output_per_million = price_per_million(value.get(output_key)?)?;
    Some(ModelPrice {
        input_per_million,
        output_per_million,
        cache_read_per_million: value.get("input_cache_read").and_then(price_per_million),
        cache_write_per_million: value.get("input_cache_write").and_then(price_per_million),
    })
}

/// 将远端每 token 价格转换为每百万 token 价格，并限制浮点噪声。
fn price_per_million(value: &Value) -> Option<f64> {
    let per_token = value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok())?;
    if !per_token.is_finite() || per_token < 0.0 {
        return None;
    }
    let per_million = per_token * 1_000_000.0;
    Some((per_million * 1_000_000_000.0).round() / 1_000_000_000.0)
}

/// 读取严格为正的整数 token 数。
fn positive_u64(value: Option<&Value>) -> Option<u64> {
    let number = value?.as_u64()?;
    (number > 0).then_some(number)
}

/// 解析 Vercel 的 effort、toggle 与 budget_tokens 推理控制。
fn parse_vercel_reasoning(row: &Value) -> Option<ModelReasoningInfo> {
    let controls = row
        .get("reasoning_options")
        .and_then(Value::as_array)
        .map(|options| parse_reasoning_controls(options))
        .unwrap_or_default();
    let supported = supported_parameter(row, "reasoning");
    if !controls.is_empty() {
        return Some(ModelReasoningInfo {
            supported: true,
            controls,
            default_effort: None,
            mandatory: None,
        });
    }
    match supported {
        Some(false) => Some(ModelReasoningInfo {
            supported: false,
            controls,
            default_effort: None,
            mandatory: None,
        }),
        // 只知道支持推理但不知道控制形式时继续查询后续目录，避免伪造强度。
        Some(true) | None => None,
    }
}

/// 解析 OpenRouter 的具体推理强度、默认值与强制状态。
fn parse_openrouter_reasoning(row: &Value) -> Option<ModelReasoningInfo> {
    if let Some(reasoning) = row.get("reasoning").and_then(Value::as_object) {
        let mut controls = Vec::new();
        if let Some(values) = reasoning.get("supported_efforts").and_then(Value::as_array) {
            let values = normalized_efforts(values.iter().filter_map(Value::as_str));
            if !values.is_empty() {
                controls.push(ModelReasoningControl::Effort { values });
            }
        }
        if reasoning
            .get("supports_max_tokens")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            controls.push(ModelReasoningControl::BudgetTokens {
                min: None,
                max: None,
            });
        }
        return Some(ModelReasoningInfo {
            supported: true,
            controls,
            default_effort: reasoning
                .get("default_effort")
                .and_then(Value::as_str)
                .map(str::to_string),
            mandatory: reasoning.get("mandatory").and_then(Value::as_bool),
        });
    }
    supported_parameter(row, "reasoning").map(|supported| ModelReasoningInfo {
        supported,
        controls: Vec::new(),
        default_effort: None,
        mandatory: None,
    })
}

/// 解析 models.dev 风格的通用推理控制数组。
fn parse_reasoning_controls(options: &[Value]) -> Vec<ModelReasoningControl> {
    let mut controls = Vec::new();
    for option in options {
        match option.get("type").and_then(Value::as_str) {
            Some("effort") => {
                let values = option
                    .get("values")
                    .and_then(Value::as_array)
                    .map(|values| normalized_efforts(values.iter().filter_map(Value::as_str)))
                    .unwrap_or_default();
                if !values.is_empty() {
                    controls.push(ModelReasoningControl::Effort { values });
                }
            }
            Some("toggle") => controls.push(ModelReasoningControl::Toggle),
            Some("budget_tokens") => controls.push(ModelReasoningControl::BudgetTokens {
                min: positive_u64(option.get("min")),
                max: positive_u64(option.get("max")),
            }),
            _ => {}
        }
    }
    controls
}

/// 将推理强度去重并按固定语义顺序排列，未知值在末尾按字典序排列。
fn normalized_efforts<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    const KNOWN_ORDER: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
    let mut unique = values
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::new();
    for known in KNOWN_ORDER {
        if unique.remove(known) {
            ordered.push(known.to_string());
        }
    }
    ordered.extend(unique);
    ordered
}

/// 判断目录是否明确列出某个受支持请求参数。
fn supported_parameter(row: &Value, parameter: &str) -> Option<bool> {
    let parameters = row.get("supported_parameters")?.as_array()?;
    Some(
        parameters
            .iter()
            .filter_map(Value::as_str)
            .any(|value| value == parameter),
    )
}

/// 校验并保留用户配置的模型标识，不做供应商推断。
fn validate_model_id(raw: &str) -> Result<String> {
    let model_id = raw.trim();
    if model_id.is_empty() || model_id.len() > 512 {
        anyhow::bail!("模型标识长度必须为 1 到 512 个字符");
    }
    if model_id.chars().any(char::is_control) {
        anyhow::bail!("模型标识不能包含控制字符");
    }
    Ok(model_id.to_string())
}

/// 返回当前唯一的模型元数据缓存路径。
fn cache_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::storage::root_dir(app)?.join(CACHE_FILE_NAME))
}

/// 读取当前版本缓存；文件不存在时返回空缓存。
fn load_cache(path: &Path) -> Result<ModelMetadataCache> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ModelMetadataCache::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("读取模型元数据失败：{}", path.display()));
        }
    };
    if bytes.len() > MAX_CACHE_BYTES {
        tracing::warn!(
            path = %path.display(),
            bytes = bytes.len(),
            "模型元数据缓存超过大小限制，本次按空缓存继续"
        );
        return Ok(ModelMetadataCache::default());
    }
    let cache: ModelMetadataCache = match serde_json::from_slice(&bytes) {
        Ok(cache) => cache,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "模型元数据缓存无效，本次按空缓存继续");
            return Ok(ModelMetadataCache::default());
        }
    };
    if cache.version != CACHE_VERSION {
        tracing::warn!(
            path = %path.display(),
            version = cache.version,
            "模型元数据缓存版本已过期，本次按空缓存继续"
        );
        return Ok(ModelMetadataCache::default());
    }
    Ok(cache)
}

/// 插入最新记录，并按更新时间稳定淘汰最旧记录。
fn insert_bounded(cache: &mut ModelMetadataCache, metadata: ModelMetadata) {
    if !cache.models.contains_key(&metadata.model_id) && cache.models.len() >= MAX_CACHED_MODELS {
        let oldest = cache
            .models
            .iter()
            .min_by(|left, right| {
                let time_order = left.1.updated_at.cmp(&right.1.updated_at);
                if time_order == Ordering::Equal {
                    left.0.cmp(right.0)
                } else {
                    time_order
                }
            })
            .map(|(model_id, _)| model_id.clone());
        if let Some(model_id) = oldest {
            cache.models.remove(&model_id);
        }
    }
    cache.models.insert(metadata.model_id.clone(), metadata);
}

/// 将模型元数据以私有权限原子写入当前唯一缓存文件。
fn save_cache(path: &Path, cache: &ModelMetadataCache) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(cache).context("序列化模型元数据失败")?;
    if bytes.len() > MAX_CACHE_BYTES {
        anyhow::bail!("模型元数据缓存超过大小限制");
    }
    crate::storage::atomic_write_private(path, &bytes)
        .with_context(|| format!("保存模型元数据缓存失败：{}", path.display()))
}

/// 返回当前 Unix 秒时间戳。
fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{
        CatalogSource, ModelMetadata, ModelPrice, ModelReasoningControl, ModelReasoningInfo,
        SOURCE_ORDER, SourceCandidate, find_model_row, model_match_rank, model_rows,
        parse_openrouter_candidate, parse_vercel_candidate,
    };
    use serde_json::json;

    /// 远端目录顺序必须始终保持 Vercel 在前、OpenRouter 在后。
    #[test]
    fn catalog_source_order_is_fixed() {
        assert_eq!(
            SOURCE_ORDER,
            [CatalogSource::Vercel, CatalogSource::OpenRouter]
        );
    }

    /// 精确模型标识必须优先于规范化尾段，尾段候选必须按标识稳定选择。
    #[test]
    fn model_match_prefers_exact_then_stable_suffix() {
        let document = json!({
            "data": [
                { "id": "zeta/claude-opus-4.6" },
                { "id": "alpha/claude-opus-4.6" },
                { "id": "claude-opus-4-6" }
            ]
        });
        let exact = find_model_row(model_rows(&document), "claude-opus-4-6").unwrap();
        assert_eq!(exact["id"], "claude-opus-4-6");

        let suffix_document = json!({
            "data": [
                { "id": "zeta/claude-opus-4.6" },
                { "id": "alpha/claude-opus-4.6" }
            ]
        });
        let suffix = find_model_row(model_rows(&suffix_document), "claude-opus-4-6").unwrap();
        assert_eq!(suffix["id"], "alpha/claude-opus-4.6");
        assert_eq!(model_match_rank("vendor/model-v11", "model-v1.1"), None);
    }

    /// Vercel 价格、上下文与三类推理控制必须转换为统一结构。
    #[test]
    fn parses_vercel_price_context_and_reasoning() {
        let document = json!({
            "data": [{
                "id": "openai/gpt-5.6-sol",
                "context_window": 1_050_000,
                "max_tokens": 128_000,
                "pricing": {
                    "input": "0.000005",
                    "output": "0.00003",
                    "input_cache_read": "0.0000005"
                },
                "supported_parameters": ["reasoning"],
                "modalities": { "input": ["text", "image"], "output": ["text"] },
                "reasoning_options": [
                    { "type": "effort", "values": ["max", "low", "medium", "low"] },
                    { "type": "toggle" },
                    { "type": "budget_tokens", "min": 1024, "max": 65536 }
                ]
            }]
        });
        let candidate = parse_vercel_candidate(&document, "gpt-5.6-sol").unwrap();
        assert_eq!(candidate.context_window, Some(1_050_000));
        assert_eq!(candidate.max_output_tokens, Some(128_000));
        assert_eq!(candidate.supports_vision, Some(true));
        assert_eq!(candidate.price.unwrap().input_per_million, 5.0);
        assert_eq!(
            candidate.reasoning.unwrap().controls,
            vec![
                ModelReasoningControl::Effort {
                    values: vec!["low".into(), "medium".into(), "max".into()]
                },
                ModelReasoningControl::Toggle,
                ModelReasoningControl::BudgetTokens {
                    min: Some(1024),
                    max: Some(65536)
                }
            ]
        );
    }

    /// 仅声明 reasoning 参数但没有控制形式时必须继续查询后续目录。
    #[test]
    fn vercel_reasoning_without_controls_stays_unresolved() {
        let document = json!({
            "data": [{
                "id": "vendor/model",
                "supported_parameters": ["reasoning"]
            }]
        });
        let candidate = parse_vercel_candidate(&document, "model").unwrap();
        assert!(candidate.reasoning.is_none());
        assert_eq!(candidate.supports_vision, None);
    }

    /// OpenRouter 的默认强度、强制状态和模型尾段必须被正确读取。
    #[test]
    fn parses_openrouter_reasoning_metadata() {
        let document = json!({
            "data": [{
                "id": "anthropic/claude-opus-4.6",
                "context_length": 1_000_000,
                "top_provider": { "max_completion_tokens": 128_000 },
                "pricing": { "prompt": "0.000005", "completion": "0.000025" },
                "architecture": { "input_modalities": ["text"], "output_modalities": ["text"] },
                "reasoning": {
                    "supported_efforts": ["max", "high", "medium", "low"],
                    "default_effort": "high",
                    "mandatory": false,
                    "supports_max_tokens": true
                }
            }]
        });
        let candidate = parse_openrouter_candidate(&document, "claude-opus-4-6").unwrap();
        assert_eq!(candidate.supports_vision, Some(false));
        let reasoning = candidate.reasoning.unwrap();
        assert_eq!(reasoning.default_effort.as_deref(), Some("high"));
        assert_eq!(reasoning.mandatory, Some(false));
        assert_eq!(
            reasoning.controls[0],
            ModelReasoningControl::Effort {
                values: vec!["low".into(), "medium".into(), "high".into(), "max".into()]
            }
        );
    }

    /// 后续目录只能补空字段，不能覆盖更早目录已经解析出的价格。
    #[test]
    fn later_catalog_only_fills_missing_fields() {
        let mut metadata = ModelMetadata::empty("model", 1);
        metadata.merge_missing(SourceCandidate {
            source: CatalogSource::Vercel,
            matched_model_id: "vendor/model".into(),
            price: Some(ModelPrice {
                input_per_million: 1.0,
                output_per_million: 2.0,
                cache_read_per_million: None,
                cache_write_per_million: None,
            }),
            context_window: None,
            max_output_tokens: None,
            reasoning: None,
            supports_vision: None,
        });
        metadata.merge_missing(SourceCandidate {
            source: CatalogSource::OpenRouter,
            matched_model_id: "other/model".into(),
            price: Some(ModelPrice {
                input_per_million: 9.0,
                output_per_million: 9.0,
                cache_read_per_million: None,
                cache_write_per_million: None,
            }),
            context_window: Some(128_000),
            max_output_tokens: Some(32_000),
            reasoning: Some(ModelReasoningInfo {
                supported: false,
                controls: Vec::new(),
                default_effort: None,
                mandatory: None,
            }),
            supports_vision: Some(true),
        });

        assert_eq!(metadata.price.unwrap().input_per_million, 1.0);
        assert_eq!(metadata.context_window, Some(128_000));
        assert_eq!(metadata.sources.price.unwrap().catalog, "vercel");
        assert_eq!(metadata.supports_vision, Some(true));
        assert_eq!(
            metadata.sources.context_window.unwrap().catalog,
            "openrouter"
        );
    }
}
