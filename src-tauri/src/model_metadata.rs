//! 模型价格、上下文容量与推理能力的本地目录。
//!
//! KeenCode 只维护一份 models.dev 公共目录的本地快照：应用启动时后台检查新鲜度，
//! 缺失或超过 24 小时才重新下载并原子替换，任何失败都保留现有快照。查询命令只
//! 读取本地文件，不在请求路径上访问网络；匹配仅按模型标识进行，不依赖自定义
//! 供应商名称。

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

use crate::http_response::{HttpResponseReadError, read_http_response_limited};

/// models.dev 公共目录快照的固定下载地址。
const CATALOG_URL: &str = "https://models.dev/api.json";
/// 当前唯一的本地模型目录快照文件名。
const CATALOG_FILE_NAME: &str = "models-dev-catalog.json";
/// 本地快照的有效期；过期后由后台任务重新下载替换。
const CATALOG_TTL_SECONDS: u64 = 24 * 60 * 60;
/// 快照下载与读取共同允许的最大字节数；当前目录约 4.5 MB 并持续增长。
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;
/// 单次批量查询允许的最大模型数量。
const MAX_QUERY_MODELS: usize = 256;
/// 目录下载连接超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// 目录完整下载超时；大文档在慢速网络下也需要一次性完成。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// 写入元数据来源的稳定目录标识。
const CATALOG_ID: &str = "models.dev";

/// 防止同一时刻重复触发目录下载；刷新期间查询继续使用现有快照。
static REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

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
    /// 用户配置的原始模型标识，也是查询键。
    pub model_id: String,
    /// 用于粗略费用统计的价格。
    pub price: Option<ModelPrice>,
    /// 模型上下文窗口 token 数。
    pub context_window: Option<u64>,
    /// 模型最大输出 token 数。
    pub max_output_tokens: Option<u64>,
    /// 模型推理支持与控制信息；为空表示未知。
    pub reasoning: Option<ModelReasoningInfo>,
    /// 是否支持图片输入；为空表示本地目录没有给出结论。
    pub supports_vision: Option<bool>,
    /// 每个字段采用的数据源。
    pub sources: ModelMetadataSources,
    /// 目录快照的更新 Unix 秒时间戳；快照缺失时为 0。
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

/// Tauri 命令：一次读取多个模型，共享同一次本地快照解析。
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

/// 应用装配时调用：后台检查快照新鲜度并按需下载，不阻塞启动。
pub fn spawn_startup_refresh(app: AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        if let Ok(path) = catalog_path(&app) {
            trigger_refresh_if_needed(&path);
        }
    });
}

/// 返回多个模型的本地快照元数据；快照缺失时字段留空，不报错也不等待网络。
fn get_many(app: &AppHandle, raw_model_ids: &[String]) -> Result<Vec<ModelMetadata>> {
    if raw_model_ids.is_empty() || raw_model_ids.len() > MAX_QUERY_MODELS {
        anyhow::bail!("模型目录批量查询数量必须为 1 到 {MAX_QUERY_MODELS}");
    }
    let model_ids = raw_model_ids
        .iter()
        .map(|model_id| validate_model_id(model_id))
        .collect::<Result<Vec<_>>>()?;
    let path = catalog_path(app)?;
    let catalog = read_catalog_document(&path);
    // 缺失、损坏或过期都在后台自愈；读取路径永远不等待网络。
    trigger_refresh_if_needed(&path);
    let updated_at = catalog.as_ref().map_or(0, |(_, updated_at)| *updated_at);
    Ok(model_ids
        .iter()
        .map(|model_id| {
            let mut metadata = ModelMetadata::empty(model_id, updated_at);
            if let Some((document, _)) = &catalog
                && let Some((row, matched_model_id)) = find_catalog_row(document, model_id)
            {
                apply_catalog_row(&mut metadata, &matched_model_id, row);
            }
            metadata
        })
        .collect())
}

/// 单模型查询复用批量读取。
fn get(app: &AppHandle, model_id: &str) -> Result<ModelMetadata> {
    get_many(app, &[model_id.to_string()])?
        .into_iter()
        .next()
        .context("模型元数据结果为空")
}

/// 快照缺失或超过有效期时在后台触发一次下载；进行中的刷新不重复触发。
fn trigger_refresh_if_needed(path: &Path) {
    if catalog_age_seconds(path).is_some_and(|age| age < CATALOG_TTL_SECONDS) {
        return;
    }
    if REFRESH_IN_FLIGHT
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let refresh_path = path.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = refresh_catalog(&refresh_path) {
            tracing::warn!(error = %error, "models.dev 模型目录刷新失败，继续使用现有快照");
        }
        REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    });
}

/// 返回快照文件的年龄秒数；文件缺失或时间不可读时返回空。
fn catalog_age_seconds(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(modified).unwrap_or_default().as_secs())
}

/// 下载新目录，完整校验通过后原子替换旧快照；失败时旧文件保持原样。
fn refresh_catalog(path: &Path) -> Result<()> {
    let bytes = download_catalog()?;
    validate_catalog_bytes(&bytes)?;
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        anyhow::bail!("模型目录目标不是可替换的普通文件：{}", path.display());
    }
    crate::storage::atomic_write_private(path, &bytes)
        .with_context(|| format!("保存模型目录快照失败：{}", path.display()))
}

/// 下载 models.dev 公共目录并限制响应体积。
fn download_catalog() -> Result<Vec<u8>> {
    let client = build_client()?;
    let response = client
        .get(CATALOG_URL)
        .header("accept", "application/json")
        .send()
        .context("请求 models.dev 模型目录失败")?
        .error_for_status()
        .context("models.dev 模型目录返回错误")?;
    match read_http_response_limited(response, MAX_CATALOG_BYTES) {
        Ok(bytes) => Ok(bytes),
        Err(HttpResponseReadError::TooLarge { .. }) => {
            anyhow::bail!("models.dev 模型目录超过大小限制")
        }
        Err(HttpResponseReadError::Read(error)) => {
            Err(error).context("读取 models.dev 模型目录失败")
        }
    }
}

/// 创建限制连接时间、完整请求时间与用户代理的目录客户端。
fn build_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent("KeenCode/0.0.1 model-metadata")
        .build()
        .context("创建模型目录 HTTP 客户端失败")
}

/// 下载内容必须先通过完整 JSON 解析与顶层结构校验，坏文档不得替换有效快照。
fn validate_catalog_bytes(bytes: &[u8]) -> Result<()> {
    let document = serde_json::from_slice::<Value>(bytes)
        .context("models.dev 目录不是有效 JSON")?;
    if !document.is_object() {
        anyhow::bail!("models.dev 目录顶层不是 JSON 对象");
    }
    Ok(())
}

/// 返回当前唯一的模型目录快照路径。
fn catalog_path(app: &AppHandle) -> Result<PathBuf> {
    Ok(crate::storage::root_dir(app)?.join(CATALOG_FILE_NAME))
}

/// 读取本地快照；缺失、非普通文件、超限或损坏都按缺失处理，由后台刷新自愈。
fn read_catalog_document(path: &Path) -> Option<(Value, u64)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let updated_at = fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    let bytes = read_catalog_bytes(path)?;
    let document = serde_json::from_slice::<Value>(&bytes).ok()?;
    document.is_object().then_some((document, updated_at))
}

/// 在读取时限制快照字节数，避免损坏或异常增长的文件耗尽内存。
fn read_catalog_bytes(path: &Path) -> Option<Vec<u8>> {
    // 用 no-follow 句柄读取，避免“检查后、打开前”路径被替换成符号链接。
    let file = crate::storage::open_readonly_regular_file(path).ok()?;
    let mut bytes = Vec::new();
    file.take((MAX_CATALOG_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_CATALOG_BYTES).then_some(bytes)
}

/// 在全部供应商中按精确、前缀精确、尾段精确、尾段规范化四个等级稳定选择模型；
/// 同分候选按供应商与模型标识字典序取最小，保证结果与遍历顺序无关。
fn find_catalog_row<'a>(
    document: &'a Value,
    model_id: &str,
) -> Option<(&'a Value, String)> {
    let mut best: Option<(u8, String, String, &'a Value)> = None;
    for (provider_key, provider) in document.as_object()? {
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (model_key, row) in models {
            let Some(rank) = catalog_match_rank(provider_key, model_key, model_id) else {
                continue;
            };
            let better = match &best {
                None => true,
                Some((best_rank, best_provider, best_model, _)) => {
                    rank < *best_rank
                        || (rank == *best_rank
                            && (provider_key.as_str(), model_key.as_str())
                                < (best_provider.as_str(), best_model.as_str()))
                }
            };
            if better {
                best = Some((rank, provider_key.clone(), model_key.clone(), row));
            }
        }
    }
    let (_, provider_key, model_key, row) = best?;
    Some((row, format!("{provider_key}/{model_key}")))
}

/// 返回单个候选的匹配等级；键与其带供应商前缀的完整标识取较优者。
fn catalog_match_rank(provider_key: &str, model_key: &str, model_id: &str) -> Option<u8> {
    let key_rank = model_match_rank(model_key, model_id);
    let full_id = format!("{provider_key}/{model_key}");
    let full_rank = model_match_rank(&full_id, model_id);
    match (key_rank, full_rank) {
        (Some(key_rank), Some(full_rank)) => Some(key_rank.min(full_rank)),
        (key_rank, full_rank) => key_rank.or(full_rank),
    }
}

/// 将 models.dev 模型行写入统一元数据；仅写入目录明确提供的字段。
fn apply_catalog_row(metadata: &mut ModelMetadata, matched_model_id: &str, row: &Value) {
    let field_source = ModelMetadataFieldSource {
        catalog: CATALOG_ID.to_owned(),
        matched_model_id: matched_model_id.to_owned(),
    };
    if let Some(price) = parse_catalog_price(row) {
        metadata.price = Some(price);
        metadata.sources.price = Some(field_source.clone());
    }
    if let Some(context_window) = row
        .get("limit")
        .and_then(|limit| positive_u64(limit.get("context")))
    {
        metadata.context_window = Some(context_window);
        metadata.sources.context_window = Some(field_source.clone());
    }
    if let Some(max_output_tokens) = row
        .get("limit")
        .and_then(|limit| positive_u64(limit.get("output")))
    {
        metadata.max_output_tokens = Some(max_output_tokens);
        metadata.sources.max_output_tokens = Some(field_source.clone());
    }
    if let Some(reasoning) = parse_catalog_reasoning(row) {
        metadata.reasoning = Some(reasoning);
        metadata.sources.reasoning = Some(field_source.clone());
    }
    if let Some(supports_vision) = input_modalities(row.get("modalities")) {
        metadata.supports_vision = Some(supports_vision);
        metadata.sources.supports_vision = Some(field_source);
    }
}

/// models.dev 的 cost 字段本身就是每百万 token 美元价格，只做数值与噪声校验。
fn parse_catalog_price(row: &Value) -> Option<ModelPrice> {
    let cost = row.get("cost")?;
    Some(ModelPrice {
        input_per_million: catalog_price(cost.get("input")?)?,
        output_per_million: catalog_price(cost.get("output")?)?,
        cache_read_per_million: cost.get("cache_read").and_then(catalog_price),
        cache_write_per_million: cost.get("cache_write").and_then(catalog_price),
    })
}

/// 读取单个价格数值，接受数字或数字字符串，并限制浮点噪声。
fn catalog_price(value: &Value) -> Option<f64> {
    let price = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))?;
    if !price.is_finite() || price < 0.0 {
        return None;
    }
    Some((price * 1_000_000_000.0).round() / 1_000_000_000.0)
}

/// reasoning 布尔缺失表示目录未知；明确支持时读取 models.dev 风格控制数组。
fn parse_catalog_reasoning(row: &Value) -> Option<ModelReasoningInfo> {
    match row.get("reasoning").and_then(Value::as_bool) {
        None => None,
        Some(false) => Some(ModelReasoningInfo {
            supported: false,
            controls: Vec::new(),
            default_effort: None,
            mandatory: None,
        }),
        Some(true) => Some(ModelReasoningInfo {
            supported: true,
            controls: row
                .get("reasoning_options")
                .and_then(Value::as_array)
                .map(|options| parse_reasoning_controls(options))
                .unwrap_or_default(),
            default_effort: None,
            mandatory: None,
        }),
    }
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

/// 返回严格为正的整数 token 数。
fn positive_u64(value: Option<&Value>) -> Option<u64> {
    let number = value?.as_u64()?;
    (number > 0).then_some(number)
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

#[cfg(test)]
mod tests {
    use super::{
        apply_catalog_row, find_catalog_row, input_modalities, model_match_rank,
        parse_catalog_price, parse_catalog_reasoning, read_catalog_document,
        validate_catalog_bytes, validate_model_id, ModelMetadata, ModelReasoningControl,
    };
    use serde_json::json;
    use std::fs;

    /// 精确标识优先，同分候选按供应商字典序稳定选择，前缀完整标识可精确命中。
    #[test]
    fn catalog_match_prefers_exact_then_stable_suffix() {
        let document = json!({
            "zeta": { "models": { "claude-opus-4.6": { "id": "zeta" } } },
            "alpha": { "models": { "claude-opus-4.6": { "id": "alpha" } } },
            "beta": { "models": { "claude-opus-4-6": { "id": "beta" } } }
        });
        let (row, matched) = find_catalog_row(&document, "claude-opus-4-6").unwrap();
        assert_eq!(row["id"], "beta");
        assert_eq!(matched, "beta/claude-opus-4-6");

        let (row, matched) = find_catalog_row(&document, "claude-opus-4.6").unwrap();
        assert_eq!(row["id"], "alpha");
        assert_eq!(matched, "alpha/claude-opus-4.6");

        let prefixed = json!({
            "openrouter": { "models": { "deepseek/deepseek-chat": { "id": "or" } } },
            "deepseek": { "models": { "deepseek-chat": { "id": "native" } } }
        });
        let (row, matched) = find_catalog_row(&prefixed, "deepseek/deepseek-chat").unwrap();
        assert_eq!(row["id"], "native");
        assert_eq!(matched, "deepseek/deepseek-chat");

        assert_eq!(model_match_rank("vendor/model-v11", "model-v1.1"), None);
    }

    /// models.dev 的价格、容量、视觉与三类推理控制必须转换为统一结构。
    #[test]
    fn parses_modelsdev_catalog_fields() {
        let row = json!({
            "id": "claude-sonnet-4-5",
            "reasoning": true,
            "reasoning_options": [
                { "type": "effort", "values": ["max", "low", "medium", "low"] },
                { "type": "toggle" },
                { "type": "budget_tokens", "min": 1024, "max": 65536 }
            ],
            "modalities": { "input": ["text", "image"], "output": ["text"] },
            "limit": { "context": 1_000_000, "output": 64_000 },
            "cost": { "input": 3, "output": "15.5", "cache_read": 0.3, "cache_write": 3.75 }
        });
        let mut metadata = ModelMetadata::empty("claude-sonnet-4-5", 7);
        apply_catalog_row(&mut metadata, "anthropic/claude-sonnet-4-5", &row);

        let price = metadata.price.unwrap();
        // models.dev 价格本身已是每百万 token 美元，不得再放大。
        assert_eq!(price.input_per_million, 3.0);
        assert_eq!(price.output_per_million, 15.5);
        assert_eq!(price.cache_read_per_million, Some(0.3));
        assert_eq!(price.cache_write_per_million, Some(3.75));
        assert_eq!(metadata.context_window, Some(1_000_000));
        assert_eq!(metadata.max_output_tokens, Some(64_000));
        assert_eq!(metadata.supports_vision, Some(true));
        assert_eq!(metadata.updated_at, 7);
        let source = metadata.sources.price.unwrap();
        assert_eq!(source.catalog, "models.dev");
        assert_eq!(source.matched_model_id, "anthropic/claude-sonnet-4-5");
        assert_eq!(
            metadata.reasoning.unwrap().controls,
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

    /// 推理字段缺失表示未知，明确关闭时给出 supported=false，开启但无选项时控制为空。
    #[test]
    fn reasoning_missing_stays_unknown_and_bool_is_explicit() {
        assert!(parse_catalog_reasoning(&json!({})).is_none());
        let disabled = parse_catalog_reasoning(&json!({ "reasoning": false })).unwrap();
        assert!(!disabled.supported);
        assert!(disabled.controls.is_empty());
        let enabled = parse_catalog_reasoning(&json!({ "reasoning": true })).unwrap();
        assert!(enabled.supported);
        assert!(enabled.controls.is_empty());
    }

    /// 目录未提供的字段必须保持未知，不得伪造默认值。
    #[test]
    fn fields_absent_from_catalog_stay_none() {
        let mut metadata = ModelMetadata::empty("m", 1);
        apply_catalog_row(&mut metadata, "vendor/m", &json!({ "id": "m" }));
        assert!(metadata.price.is_none());
        assert!(metadata.context_window.is_none());
        assert!(metadata.max_output_tokens.is_none());
        assert!(metadata.reasoning.is_none());
        assert!(metadata.supports_vision.is_none());
        assert_eq!(metadata.sources.price, None);
        assert!(parse_catalog_price(&json!({ "cost": { "input": 1 } })).is_none());
    }

    /// 视觉能力只在目录明确声明 image 输入模态时为真，声明其他模态时为假。
    #[test]
    fn vision_requires_declared_image_modality() {
        let text_only = json!({ "modalities": { "input": ["text"] } });
        assert_eq!(input_modalities(text_only.get("modalities")), Some(false));
        assert_eq!(input_modalities(Some(&json!({}))), None);
    }

    /// 缺失或损坏的快照都必须按缺失处理，由后台刷新自愈而不是让查询崩溃。
    #[test]
    fn missing_or_corrupt_catalog_reads_as_none() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("models-dev-catalog.json");
        assert!(read_catalog_document(&path).is_none());

        fs::write(&path, b"not-json").unwrap();
        assert!(read_catalog_document(&path).is_none());
        // 有效的顶层对象可以读取。
        fs::write(&path, br#"{"anthropic":{"models":{}}}"#).unwrap();
        assert!(read_catalog_document(&path).is_some());
    }

    /// 下载内容必须在落盘前通过 JSON 与顶层结构校验。
    #[test]
    fn invalid_catalog_bytes_are_rejected() {
        assert!(validate_catalog_bytes(b"not-json").is_err());
        assert!(validate_catalog_bytes(b"[1,2]").is_err());
        assert!(validate_catalog_bytes(br#"{"anthropic":{"models":{}}}"#).is_ok());
    }

    /// 空标识、超长标识与控制字符必须被拒绝。
    #[test]
    fn model_id_validation_rejects_invalid_input() {
        assert!(validate_model_id("  ").is_err());
        assert!(validate_model_id("a\u{0}b").is_err());
        assert!(validate_model_id("x".repeat(513).as_str()).is_err());
        assert_eq!(validate_model_id(" claude-sonnet-4-5 ").unwrap(), "claude-sonnet-4-5");
    }
}
