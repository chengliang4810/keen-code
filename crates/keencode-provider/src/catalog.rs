use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use keencode_model::ModelError;
use reqwest::{Method, Response, Url};
use serde_json::Value;

use crate::client::ProviderClient;
use crate::http::{decode_error_response, transport_error};

/// 模型目录中单个模型标识允许的最大 UTF-8 字节数。
const MAX_MODEL_ID_BYTES: usize = 2 * 1024;
/// 模型目录分页游标允许的最大 UTF-8 字节数。
const MAX_CATALOG_CURSOR_BYTES: usize = 8 * 1024;
/// 服务端显式 next URL 允许的最大 UTF-8 字节数。
const MAX_CATALOG_NEXT_URL_BYTES: usize = 16 * 1024;

/// 模型目录中的一个精确模型标识及其公开元数据。
#[derive(Clone, PartialEq)]
pub struct ModelCatalogEntry {
    /// Provider 返回且未经语义改写的精确模型标识。
    pub id: String,
    /// 去重前相同模型标识在目录中出现的次数。
    pub source_count: usize,
    /// Provider 返回的非认证目录元数据；字符串条目时为 `Null`。
    pub metadata: Value,
}

impl std::fmt::Debug for ModelCatalogEntry {
    /// 避免把不可信目录元数据或控制字符直接写入调试日志。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelCatalogEntry")
            .field("id", &safe_debug_text(&self.id))
            .field("source_count", &self.source_count)
            .field("metadata", &"<untrusted-metadata>")
            .finish()
    }
}

/// 一个 Provider 全部分页归并后的模型目录。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelCatalog {
    /// 实际读取的目录页数。
    pub pages: usize,
    /// 去重前读取的原始条目数。
    pub raw_count: usize,
    /// 缺少稳定 ID 而无法解析的条目数。
    pub invalid_count: usize,
    /// 全部分页在 HTTP 线上读取的累计正文大小。
    pub wire_bytes: usize,
    /// 按首次出现顺序排列且精确 ID 去重的条目。
    pub models: Vec<ModelCatalogEntry>,
}

/// 模型目录未完整读取时保留已成功分页及统一错误。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelCatalogFailure {
    /// 失败发生前已经完整解析的全部分页事实。
    pub partial: ModelCatalog,
    /// 阻止目录完整读取的 Provider 中立错误。
    pub error: ModelError,
}

impl fmt::Display for ModelCatalogFailure {
    /// 输出不包含目录元数据和认证信息的失败说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "模型目录未完整读取：{}", self.error)
    }
}

impl Error for ModelCatalogFailure {
    /// 返回阻止目录完整读取的统一底层错误。
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

/// 一个目录页的解析结果和下一页地址。
#[derive(Debug)]
pub(crate) struct CatalogPage {
    /// 当前页中成功解析的精确模型标识与原条目。
    pub(crate) entries: Vec<(String, Value)>,
    /// 当前页缺少稳定模型标识的条目数。
    pub(crate) invalid_count: usize,
    /// 已经过同源与基础路径校验的下一页地址。
    pub(crate) next_url: Option<Url>,
}

/// 请求、分页并归并当前 Provider 的实时模型目录。
pub(crate) async fn fetch_model_catalog(
    client: &ProviderClient,
) -> Result<ModelCatalog, ModelCatalogFailure> {
    let mut merged = BTreeMap::<String, ModelCatalogEntry>::new();
    let mut order = Vec::new();
    let mut catalog = ModelCatalog::default();
    let mut url = match client.config().models_url() {
        Ok(url) => url,
        Err(error) => {
            return Err(catalog_failure(
                catalog,
                merged,
                order,
                ModelError::InvalidRequest {
                    message: error.to_string(),
                },
            ));
        }
    };
    let origin = url.origin().ascii_serialization();
    let base_path = client.config().base_url().path().to_owned();
    let mut visited = BTreeSet::new();

    loop {
        if catalog.pages >= client.config().max_catalog_pages {
            return Err(catalog_failure(
                catalog,
                merged,
                order,
                ModelError::Protocol {
                    message: format!(
                        "模型目录分页超过 {} 页安全上限",
                        client.config().max_catalog_pages
                    ),
                },
            ));
        }
        if !visited.insert(url.as_str().to_owned()) {
            return Err(catalog_failure(
                catalog,
                merged,
                order,
                ModelError::Protocol {
                    message: "模型目录返回了重复分页游标".to_owned(),
                },
            ));
        }
        let request = match client.authenticated_request(Method::GET, url.clone()) {
            Ok(request) => request,
            Err(error) => return Err(catalog_failure(catalog, merged, order, error)),
        };
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                return Err(catalog_failure(
                    catalog,
                    merged,
                    order,
                    transport_error(error, client.config().api_key()),
                ));
            }
        };
        if !response.status().is_success() {
            let error = decode_error_response(
                response,
                client.config().api_key(),
                client.config().max_event_bytes,
                #[cfg(feature = "live-test-trace")]
                None,
            )
            .await;
            return Err(catalog_failure(catalog, merged, order, error));
        }
        let (value, page_bytes) =
            match read_catalog_json(response, client.config().max_event_bytes).await {
                Ok(page) => page,
                Err(error) => return Err(catalog_failure(catalog, merged, order, error)),
            };
        catalog.wire_bytes = match catalog.wire_bytes.checked_add(page_bytes) {
            Some(bytes) => bytes,
            None => {
                return Err(catalog_failure(
                    catalog,
                    merged,
                    order,
                    ModelError::Protocol {
                        message: "模型目录累计响应长度溢出".to_owned(),
                    },
                ));
            }
        };
        if catalog.wire_bytes > client.config().max_catalog_bytes {
            return Err(catalog_failure(
                catalog,
                merged,
                order,
                ModelError::Protocol {
                    message: format!(
                        "模型目录累计响应超过 {} 字节安全上限",
                        client.config().max_catalog_bytes
                    ),
                },
            ));
        }
        let page = match parse_catalog_page(value, &url, &origin, &base_path) {
            Ok(page) => page,
            Err(error) => return Err(catalog_failure(catalog, merged, order, error)),
        };
        catalog.pages += 1;
        catalog.raw_count += page.entries.len() + page.invalid_count;
        catalog.invalid_count += page.invalid_count;
        for (id, metadata) in page.entries {
            if let Some(existing) = merged.get_mut(&id) {
                existing.source_count += 1;
            } else {
                order.push(id.clone());
                merged.insert(
                    id.clone(),
                    ModelCatalogEntry {
                        id,
                        source_count: 1,
                        metadata,
                    },
                );
            }
        }
        let Some(next_url) = page.next_url else {
            break;
        };
        url = next_url;
    }

    Ok(finalize_catalog(catalog, merged, order))
}

/// 把已成功分页的去重模型按首次出现顺序固定到目录结果中。
fn finalize_catalog(
    mut catalog: ModelCatalog,
    mut merged: BTreeMap<String, ModelCatalogEntry>,
    order: Vec<String>,
) -> ModelCatalog {
    catalog.models = order
        .into_iter()
        .filter_map(|id| merged.remove(&id))
        .collect();
    catalog
}

/// 构造携带成功分页事实的目录失败，供调用方持久化并恢复。
fn catalog_failure(
    catalog: ModelCatalog,
    merged: BTreeMap<String, ModelCatalogEntry>,
    order: Vec<String>,
    error: ModelError,
) -> ModelCatalogFailure {
    ModelCatalogFailure {
        partial: finalize_catalog(catalog, merged, order),
        error,
    }
}

/// 解析顶层数组、`data` 数组或 `models` 数组及其分页字段。
pub(crate) fn parse_catalog_page(
    value: Value,
    current_url: &Url,
    expected_origin: &str,
    base_path: &str,
) -> Result<CatalogPage, ModelError> {
    let (items, pagination) = match value {
        Value::Array(items) => (items, None),
        Value::Object(mut object) => {
            let items = object
                .remove("data")
                .or_else(|| object.remove("models"))
                .and_then(|items| match items {
                    Value::Array(items) => Some(items),
                    _ => None,
                })
                .ok_or_else(|| ModelError::Protocol {
                    message: "模型目录必须是顶层数组、data 数组或 models 数组".to_owned(),
                })?;
            (items, Some(object))
        }
        _ => {
            return Err(ModelError::Protocol {
                message: "模型目录顶层必须是数组或对象".to_owned(),
            });
        }
    };

    let mut entries = Vec::new();
    let mut invalid_count = 0;
    for item in items {
        let id = match &item {
            Value::String(id) => Some(id.as_str()),
            Value::Object(object) => object
                .get("id")
                .or_else(|| object.get("model"))
                .and_then(Value::as_str),
            _ => None,
        };
        let Some(id) = id.filter(|id| valid_model_id(id)) else {
            invalid_count += 1;
            continue;
        };
        entries.push((id.to_owned(), item));
    }

    let next_url = pagination
        .as_ref()
        .map(|object| next_catalog_url(object, current_url, expected_origin, base_path))
        .transpose()?
        .flatten();
    Ok(CatalogPage {
        entries,
        invalid_count,
        next_url,
    })
}

/// 解析常见 next URL、next cursor 或 Anthropic `last_id` 分页形式。
fn next_catalog_url(
    object: &serde_json::Map<String, Value>,
    current_url: &Url,
    expected_origin: &str,
    base_path: &str,
) -> Result<Option<Url>, ModelError> {
    if let Some(next) = object
        .get("next")
        .and_then(Value::as_str)
        .filter(|next| !next.trim().is_empty())
    {
        if next.len() > MAX_CATALOG_NEXT_URL_BYTES || next.chars().any(char::is_control) {
            return Err(ModelError::Protocol {
                message: "模型目录 next 地址超过安全上限或包含控制字符".to_owned(),
            });
        }
        let next_url = current_url
            .join(next)
            .map_err(|error| ModelError::Protocol {
                message: format!("模型目录 next 地址无效：{error}"),
            })?;
        validate_next_url(&next_url, expected_origin, base_path)?;
        return Ok(Some(next_url));
    }
    if let Some(cursor) = object
        .get("next_cursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.trim().is_empty())
    {
        validate_cursor(cursor)?;
        let mut next_url = current_url.clone();
        replace_query_pair(&mut next_url, "after", cursor);
        return Ok(Some(next_url));
    }
    let has_more = object
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if has_more {
        let cursor = object
            .get("last_id")
            .and_then(Value::as_str)
            .filter(|cursor| !cursor.trim().is_empty())
            .ok_or_else(|| ModelError::Protocol {
                message: "模型目录声明 has_more 但未提供 next、next_cursor 或 last_id".to_owned(),
            })?;
        validate_cursor(cursor)?;
        let mut next_url = current_url.clone();
        replace_query_pair(&mut next_url, "after_id", cursor);
        return Ok(Some(next_url));
    }
    Ok(None)
}

/// 确保下一页不会把认证 Header 发送到其他源站或基础路径之外。
fn validate_next_url(
    next_url: &Url,
    expected_origin: &str,
    base_path: &str,
) -> Result<(), ModelError> {
    if next_url.origin().ascii_serialization() != expected_origin
        || !next_url.path().starts_with(base_path)
        || !next_url.username().is_empty()
        || next_url.password().is_some()
        || next_url.fragment().is_some()
    {
        return Err(ModelError::Protocol {
            message: "模型目录 next 地址越过了 Provider 源站或基础路径边界".to_owned(),
        });
    }
    Ok(())
}

/// 替换指定分页查询参数并保留其他非认证参数。
fn replace_query_pair(url: &mut Url, name: &str, value: &str) {
    let existing = url
        .query_pairs()
        .filter(|(key, _)| key != name)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in existing {
            pairs.append_pair(&key, &value);
        }
        pairs.append_pair(name, value);
    }
}

/// 在内存上限内读取并解析模型目录 JSON。
async fn read_catalog_json(
    mut response: Response,
    max_bytes: usize,
) -> Result<(Value, usize), ModelError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ModelError::Transport {
            message: error.to_string(),
            retryable: true,
        })?
    {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| ModelError::Protocol {
                message: "模型目录响应长度溢出".to_owned(),
            })?;
        if next_len > max_bytes {
            return Err(ModelError::Protocol {
                message: format!("模型目录响应超过 {max_bytes} 字节安全上限"),
            });
        }
        body.extend_from_slice(&chunk);
    }
    let length = body.len();
    let value = serde_json::from_slice(&body).map_err(|error| ModelError::Protocol {
        message: format!("模型目录不是有效 JSON：{error}"),
    })?;
    Ok((value, length))
}

/// 判断模型标识是否适合持久化、日志摘要和后续请求使用。
fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id == id.trim()
        && id.len() <= MAX_MODEL_ID_BYTES
        && !id.chars().any(char::is_control)
}

/// 校验服务端分页游标不会制造超大 URL 或日志控制字符。
fn validate_cursor(cursor: &str) -> Result<(), ModelError> {
    if cursor.len() > MAX_CATALOG_CURSOR_BYTES || cursor.chars().any(char::is_control) {
        return Err(ModelError::Protocol {
            message: "模型目录分页游标超过安全上限或包含控制字符".to_owned(),
        });
    }
    Ok(())
}

/// 为调试输出生成单行且有界的不可信文本摘要。
fn safe_debug_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect()
}
