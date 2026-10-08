use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const CATALOG_URL: &str = "https://models.dev/api.json";
const CACHE_PATH: &str = "cache/models/models.dev.json";
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_MODELS: usize = 50_000;
const FRESH_MS: u64 = 60 * 60 * 1000;

#[derive(Default)]
pub struct ModelCatalogState(Mutex<Option<Instant>>);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSnapshot {
    schema_version: u8,
    fetched_at: u64,
    providers: Vec<CatalogProvider>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogProvider {
    id: String,
    api: Option<String>,
    models: Vec<CatalogModel>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogModel {
    id: String,
    canonical_id: Option<String>,
    context_limit: u64,
    output_limit: Option<u64>,
    vision: Option<bool>,
    reasoning: Option<bool>,
    reasoning_levels: Option<Vec<String>>,
}

fn reasoning_levels(model: &Value) -> Option<Vec<String>> {
    if model.get("reasoning").and_then(Value::as_bool) == Some(false) {
        return Some(Vec::new());
    }
    let options = model.get("reasoning_options")?.as_array()?;
    let toggle = options
        .iter()
        .any(|option| option.get("type").and_then(Value::as_str) == Some("toggle"));
    let mut levels = Vec::new();
    if toggle {
        levels.push("none".to_owned());
    }
    for option in options
        .iter()
        .filter(|option| option.get("type").and_then(Value::as_str) == Some("effort"))
    {
        for level in option
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(level) = level
                .as_str()
                .filter(|level| valid_id(level) && level.len() <= 64 && level.trim() == *level)
            {
                if !levels.iter().any(|existing| existing == level) && levels.len() < 16 {
                    levels.push(level.to_owned());
                }
            }
        }
    }
    if toggle && levels.len() == 1 {
        levels.push("enabled".to_owned());
    }
    (!levels.is_empty()).then_some(levels)
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn valid_api(value: &str) -> bool {
    value.len() <= 2048
        && reqwest::Url::parse(value).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        })
}

fn parse_catalog(bytes: &[u8], fetched_at: u64) -> Result<CatalogSnapshot, String> {
    if bytes.len() > MAX_BYTES {
        return Err("Model catalog exceeds the size limit.".into());
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "Invalid model catalog JSON.")?;
    let source = value.as_object().ok_or("Invalid model catalog format.")?;
    if source.len() > 2048 {
        return Err("Too many model providers.".into());
    }
    let mut providers = Vec::new();
    let mut count = 0;
    for (id, provider) in source {
        if !valid_id(id) {
            continue;
        }
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        count += models.len();
        if count > MAX_MODELS {
            return Err("Too many catalog models.".into());
        }
        let models: Vec<_> = models
            .iter()
            .filter_map(|(id, model)| {
                if !valid_id(id) {
                    return None;
                }
                let context_limit = model.get("limit")?.get("context")?.as_u64()?;
                if !(8192..=4_000_000).contains(&context_limit) {
                    return None;
                }
                Some(CatalogModel {
                    id: id.clone(),
                    canonical_id: model
                        .get("canonical_model_id")
                        .and_then(Value::as_str)
                        .filter(|id| valid_id(id))
                        .map(str::to_owned),
                    context_limit,
                    output_limit: model
                        .get("limit")
                        .and_then(|limit| limit.get("output"))
                        .and_then(Value::as_u64)
                        .filter(|limit| *limit > 0 && *limit <= 4_000_000),
                    vision: model
                        .get("modalities")
                        .and_then(|modalities| modalities.get("input"))
                        .and_then(Value::as_array)
                        .filter(|inputs| !inputs.is_empty() && inputs.iter().all(Value::is_string))
                        .map(|inputs| inputs.iter().any(|input| input.as_str() == Some("image"))),
                    reasoning: model.get("reasoning").and_then(Value::as_bool),
                    reasoning_levels: reasoning_levels(model),
                })
            })
            .collect();
        if !models.is_empty() {
            providers.push(CatalogProvider {
                id: id.clone(),
                api: provider
                    .get("api")
                    .and_then(Value::as_str)
                    .filter(|api| valid_api(api))
                    .map(str::to_owned),
                models,
            });
        }
    }
    if providers.is_empty() {
        return Err("Model catalog contains no supported models.".into());
    }
    Ok(CatalogSnapshot {
        schema_version: 1,
        fetched_at,
        providers,
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn read_cache_at(path: &Path) -> Result<Option<CatalogSnapshot>, String> {
    super::storage::reject_link(path)?;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
        return Err("Invalid model catalog cache file.".into());
    }
    let fetched_at = metadata
        .modified()
        .map_err(|e| e.to_string())?
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as u64;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    parse_catalog(&bytes, fetched_at).map(Some)
}

fn write_cache_at(path: &Path, bytes: &[u8]) -> Result<(), String> {
    super::storage::reject_link(path)?;
    let parent = path.parent().ok_or("Invalid model catalog cache path.")?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

fn decode_body(bytes: Vec<u8>, gzip: bool) -> Result<Vec<u8>, String> {
    if !gzip {
        return Ok(bytes);
    }
    let mut output = Vec::new();
    GzDecoder::new(bytes.as_slice())
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|_| "Invalid compressed model catalog.")?;
    if output.len() > MAX_BYTES {
        return Err("Model catalog exceeds the size limit.".into());
    }
    Ok(output)
}

struct CatalogDownload {
    snapshot: CatalogSnapshot,
    bytes: Vec<u8>,
}

async fn download_catalog() -> Result<CatalogDownload, String> {
    tokio::time::timeout(Duration::from_secs(120), async {
        let client = super::net::agent_http_client(CATALOG_URL, false).await?;
        let request = client
            .get(CATALOG_URL)
            .header(reqwest::header::USER_AGENT, "RCode/0.9.0")
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::ACCEPT_ENCODING, "gzip");
        let mut response = request
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "Model catalog HTTP {}.",
                response.status().as_u16()
            ));
        }
        let gzip = match response.headers().get(reqwest::header::CONTENT_ENCODING) {
            None => false,
            Some(value) if value == "gzip" => true,
            Some(value) if value == "identity" => false,
            _ => return Err("Unsupported model catalog encoding.".into()),
        };
        if response
            .content_length()
            .is_some_and(|size| size > MAX_BYTES as u64)
        {
            return Err("Model catalog exceeds the size limit.".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| e.without_url().to_string())?
        {
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Err("Model catalog exceeds the size limit.".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        tokio::task::spawn_blocking(move || {
            let bytes = decode_body(bytes, gzip)?;
            let snapshot = parse_catalog(&bytes, now_ms())?;
            Ok(CatalogDownload { snapshot, bytes })
        })
        .await
        .map_err(|e| e.to_string())?
    })
    .await
    .map_err(|_| "Model catalog download timed out.".to_string())?
}

#[tauri::command]
pub async fn model_catalog_load() -> Result<Option<CatalogSnapshot>, String> {
    tokio::task::spawn_blocking(|| read_cache_at(&super::storage::path(CACHE_PATH)?))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn model_catalog_refresh(
    state: tauri::State<'_, ModelCatalogState>,
    force: Option<bool>,
) -> Result<CatalogSnapshot, String> {
    let mut last_attempt = state.0.lock().await;
    let cached = model_catalog_load().await.unwrap_or_else(|error| {
        log::warn!("Model catalog cache: {error}");
        None
    });
    let force = force.unwrap_or(false);
    if !force {
        if let Some(snapshot) = cached
            .as_ref()
            .filter(|s| now_ms().saturating_sub(s.fetched_at) < FRESH_MS)
        {
            return Ok(snapshot.clone());
        }
    }
    let cooldown = Duration::from_secs(if force { 5 } else { 300 });
    if last_attempt.is_some_and(|time| time.elapsed() < cooldown) {
        return cached.ok_or("Model catalog refresh is cooling down. Try again shortly.".into());
    }
    let result = download_catalog().await;
    *last_attempt = Some(Instant::now());
    let download = result?;
    tokio::task::spawn_blocking(move || {
        super::storage::directory("cache/models")?;
        write_cache_at(&super::storage::path(CACHE_PATH)?, &download.bytes)?;
        Ok(download.snapshot)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_capabilities_and_canonical_ids_without_accepting_invalid_limits() {
        let bytes = serde_json::to_vec(&json!({"deepseek": {
            "api": "https://api.deepseek.com", "env": ["IGNORED_KEY"], "models": {
                "deepseek-flash": {"canonical_model_id": "deepseek/deepseek-v4.1-flash",
                    "limit": {"context": 1000000, "output": 393216},
                    "modalities": {"input": ["text", "image"]}, "reasoning": true},
                "text-only": {"limit": {"context": 32000}, "modalities": {"input": ["text"]}, "reasoning": false},
                "bad": {"limit": {"context": -1}}, "missing": {},
                "unsupported": {"limit": {"context": 999999999}}
            }}})).unwrap();
        let snapshot = parse_catalog(&bytes, now_ms()).unwrap();
        let models = &snapshot.providers[0].models;
        assert_eq!(models.len(), 2);
        let flash = &models[0];
        assert_eq!(flash.context_limit, 1_000_000);
        assert_eq!(flash.output_limit, Some(393_216));
        assert_eq!(flash.vision, Some(true));
        assert_eq!(
            flash.canonical_id.as_deref(),
            Some("deepseek/deepseek-v4.1-flash")
        );
        assert_eq!(models[1].vision, Some(false));
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains("IGNORED_KEY"));
        for invalid in [b"{}".as_slice(), b"[]", b"broken"] {
            assert!(parse_catalog(invalid, 0).is_err());
        }
        assert!(parse_catalog(&vec![b' '; MAX_BYTES + 1], 0).is_err());
    }

    #[test]
    fn cache_round_trip_validates_schema_and_does_not_replace_good_data_on_parse_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        assert!(read_cache_at(&path).unwrap().is_none());
        let bytes = br#"{"p":{"name":"Keep all fields","models":{"m":{"limit":{"context":32000},"cost":{"input":0.5}}}}}"#;
        write_cache_at(&path, bytes).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            read_cache_at(&path).unwrap().unwrap().providers[0].models[0].context_limit,
            32000
        );
        assert!(parse_catalog(b"bad", 0).is_err());
        assert!(read_cache_at(&path).unwrap().is_some());
        std::fs::write(&path, b"invalid").unwrap();
        assert!(read_cache_at(&path).is_err());
    }

    #[test]
    fn gzip_decoding_is_bounded() {
        fn gzip(bytes: &[u8]) -> Vec<u8> {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
        assert_eq!(decode_body(gzip(b"catalog"), true).unwrap(), b"catalog");
        assert!(decode_body(gzip(&vec![b' '; MAX_BYTES + 1]), true).is_err());
        assert!(decode_body(b"invalid".to_vec(), true).is_err());
    }

    #[tokio::test]
    #[ignore = "Public models.dev network integration"]
    async fn downloads_public_catalog() {
        let download = download_catalog().await.unwrap();
        let snapshot = &download.snapshot;
        assert!(snapshot.providers.iter().any(|p| p.id == "deepseek"));
        println!(
            "Downloaded {} providers and {} models",
            snapshot.providers.len(),
            snapshot
                .providers
                .iter()
                .map(|p| p.models.len())
                .sum::<usize>()
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.dev.json");
        write_cache_at(&path, &download.bytes).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), download.bytes);
        assert_eq!(
            read_cache_at(&path).unwrap().unwrap().providers.len(),
            snapshot.providers.len()
        );
    }
}
