//! 原生工作台的本地活动统计。

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::native_paths::NativePaths;

const MAX_PROFILE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileStore {
    prompts: u64,
    tokens: u64,
    model_counts: BTreeMap<String, u64>,
    skill_counts: BTreeMap<String, u64>,
    day_counts: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeProfile {
    pub generated_at_unix_ms: u64,
    pub prompts: u64,
    pub tokens: u64,
    pub model_counts: BTreeMap<String, u64>,
    pub skill_counts: BTreeMap<String, u64>,
    pub day_counts: BTreeMap<String, u64>,
}

#[derive(Clone)]
pub struct ProfileService {
    paths: Arc<NativePaths>,
    store: Arc<Mutex<ProfileStore>>,
}

impl ProfileService {
    pub fn new(paths: Arc<NativePaths>) -> Self {
        let store = load_store(&paths.data_root).unwrap_or_default();
        Self {
            paths,
            store: Arc::new(Mutex::new(store)),
        }
    }

    pub fn record_prompt(&self, model: Option<&str>, skill: Option<&str>, time_unix_ms: u64) {
        let mut store = self.store.lock();
        store.prompts = store.prompts.saturating_add(1);
        if let Some(model) = model.filter(|value| !value.is_empty()) {
            *store.model_counts.entry(model.to_owned()).or_default() += 1;
        }
        if let Some(skill) = skill.filter(|value| !value.is_empty()) {
            *store.skill_counts.entry(skill.to_owned()).or_default() += 1;
        }
        *store.day_counts.entry(day_key(time_unix_ms)).or_default() += 1;
        let _ = save_store(&self.paths.data_root, &store);
    }

    pub fn record_tokens(&self, tokens: u64) {
        let mut store = self.store.lock();
        store.tokens = store.tokens.saturating_add(tokens);
        let _ = save_store(&self.paths.data_root, &store);
    }

    pub fn snapshot(&self) -> NativeProfile {
        let store = self.store.lock().clone();
        NativeProfile {
            generated_at_unix_ms: now_unix_ms(),
            prompts: store.prompts,
            tokens: store.tokens,
            model_counts: store.model_counts,
            skill_counts: store.skill_counts,
            day_counts: store.day_counts,
        }
    }
}

fn store_path(data_root: &Path) -> PathBuf {
    data_root.join("native-profile.json")
}

fn load_store(data_root: &Path) -> Result<ProfileStore, String> {
    let path = store_path(data_root);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProfileStore::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err("Profile 统计文件超过大小限制".to_owned());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn save_store(data_root: &Path, store: &ProfileStore) -> Result<(), String> {
    fs::create_dir_all(data_root).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(store).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err("Profile 统计文件超过大小限制".to_owned());
    }
    let path = store_path(data_root);
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn day_key(time_unix_ms: u64) -> String {
    // 只用于趋势分组；UTC 事实时间由调用者保存，避免把本机偏移写回事实。
    let seconds = i64::try_from(time_unix_ms / 1000).unwrap_or(i64::MAX);
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|value| value.date_naive().to_string())
        .unwrap_or_else(|| "unknown".to_owned())
}
