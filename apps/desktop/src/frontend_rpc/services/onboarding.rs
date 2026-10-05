//! 本地首次引导记录服务。
//!
//! 源服务把记录定义为设备级 v2 JSON；桌面端保留同一字段和方法语义，
//! 但把文件放在 KeenCode 数据根，并用本地生成的匿名身份替代账号/云端身份。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use tauri::AppHandle;

const RECORD_FILE: &str = "onboarding-record.json";
const ANONYMOUS_ID_FILE: &str = "onboarding-anonymous-id";
const MAX_RECORD_BYTES: u64 = 1024 * 1024;
const LOCAL_ANONYMOUS_PREFIX: &str = "local-anonymous-";

static STORAGE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum InterfaceMode {
    Coding,
    Office,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum UploadState {
    Pending,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecordEntry {
    user_id: Option<String>,
    occupation: Option<String>,
    interface_mode: Option<InterfaceMode>,
    memory_enabled: Option<bool>,
    proactive_suggestions_enabled: Option<bool>,
    completed_at: String,
    upload_state: UploadState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum DecisionStatus {
    Dismissed,
    #[serde(rename = "existing_local_user")]
    ExistingLocalUser,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DecisionReason {
    UserClosed,
    ExistingLocalTask,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Decision {
    user_id: Option<String>,
    status: DecisionStatus,
    reason: DecisionReason,
    decided_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordFile {
    version: u8,
    device_mid: String,
    entries: Vec<RecordEntry>,
    decisions: Vec<Decision>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
// 源 onboardingRecordFileSchema 明确接受 v1/v2；这里只在内存补空 decisions，
// 不主动迁移或覆盖旧文件，下一次真实写入才按当前 v2 结构原子保存。
struct RecordFileV1 {
    version: u8,
    device_mid: String,
    entries: Vec<RecordEntry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecordFileV2 {
    version: u8,
    device_mid: String,
    entries: Vec<RecordEntry>,
    decisions: Vec<Decision>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EntryInput {
    occupation: Option<String>,
    interface_mode: Option<InterfaceMode>,
    memory_enabled: Option<bool>,
    proactive_suggestions_enabled: Option<bool>,
    completed_at: String,
}

pub(crate) fn call(app: &AppHandle, method: &str, args: Value) -> Result<Value, String> {
    match method {
        "appendRecord" => append_record(app, &args),
        "claimAnonymousRecord" => {
            reject_unknown(&args, &[], method)?;
            claim_anonymous_record(app).map(|()| Value::Null)
        }
        "shouldOnboard" => {
            let device_mid = required_string(&args, "deviceMid")?;
            should_onboard(app, &device_mid).map(Value::Bool)
        }
        "dismissOnboarding" => {
            let device_mid = required_string(&args, "deviceMid")?;
            dismiss_onboarding(app, &device_mid).map(|()| Value::Null)
        }
        "getLatestEntry" => {
            reject_unknown(&args, &[], method)?;
            get_latest_entry(app).map(to_value)?
        }
        "syncSettingsFromRecord" => {
            reject_unknown(&args, &[], method)?;
            sync_settings_from_record(app).map(to_value)?
        }
        "updateRecordPreferences" => {
            let patch = args.get("patch").unwrap_or(&args);
            update_record_preferences(app, patch).map(|()| Value::Null)
        }
        "getRecords" => {
            reject_unknown(&args, &[], method)?;
            get_records(app).map(to_value)?
        }
        "clearRecords" => {
            reject_unknown(&args, &[], method)?;
            clear_records(app).map(|()| Value::Null)
        }
        _ => Err(format!("未知服务方法：onboarding-record.{method}")),
    }
}

fn append_record(app: &AppHandle, args: &Value) -> Result<Value, String> {
    let device_mid = required_string(args, "deviceMid")?;
    let entry = parse_entry_input(
        args.get("entry")
            .ok_or_else(|| "缺少参数 entry".to_owned())?,
    )?;
    let user_id = local_anonymous_id(app)?;
    with_storage_lock(|| {
        let path = record_path(app)?;
        let mut file = read_record_path(&path)?.unwrap_or_else(|| create_file(&device_mid));
        // 文件内 deviceMid 是设备事实源；设备标识变化不能把同一份记录复制成第二份。
        let record = RecordEntry {
            user_id: Some(user_id.clone()),
            occupation: entry.occupation,
            interface_mode: entry.interface_mode,
            memory_enabled: entry.memory_enabled,
            proactive_suggestions_enabled: entry.proactive_suggestions_enabled,
            completed_at: entry.completed_at,
            upload_state: UploadState::Pending,
        };
        let changed_entry = upsert_entry(&mut file, record);
        let changed_decisions = file
            .decisions
            .iter()
            .any(|decision| decision.user_id.as_deref() == Some(user_id.as_str()));
        file.decisions
            .retain(|decision| decision.user_id.as_deref() != Some(user_id.as_str()));
        if changed_entry || changed_decisions || !path.exists() {
            write_record_path(&path, &file)?;
        }
        Ok(Value::Null)
    })
}

fn claim_anonymous_record(app: &AppHandle) -> Result<(), String> {
    let user_id = local_anonymous_id(app)?;
    with_storage_lock(|| {
        let path = record_path(app)?;
        let Some(mut file) = read_record_path(&path)? else {
            return Ok(());
        };
        if has_identity_record(&file, &user_id) {
            return Ok(());
        }
        // 兼容旧版 null 身份，始终只认领最后一条，避免产生重复本地身份记录。
        if let Some(entry) = file
            .entries
            .iter_mut()
            .rev()
            .find(|entry| entry.user_id.is_none())
        {
            entry.user_id = Some(user_id);
            write_record_path(&path, &file)?;
            return Ok(());
        }
        if let Some(decision) = file
            .decisions
            .iter_mut()
            .rev()
            .find(|decision| decision.user_id.is_none())
        {
            decision.user_id = Some(user_id);
            write_record_path(&path, &file)?;
        }
        Ok(())
    })
}

fn should_onboard(app: &AppHandle, device_mid: &str) -> Result<bool, String> {
    let user_id = local_anonymous_id(app)?;
    let path = record_path(app)?;
    if let Some(file) = read_record_path(&path)?
        && has_local_identity_record(&file, &user_id)
    {
        return Ok(false);
    }

    // 引导服务不能把“存在本地任务”写成前端设置猜测；直接读取 Runtime 的真实存储索引。
    let has_existing_local_task = crate::require_owned_runtime(app)
        .map_err(|error| error.to_string())?
        .stored_sessions()
        .map_err(|error| format!("读取本地任务索引失败：{error}"))?
        .into_iter()
        .any(|session| !session.corrupt);
    if !has_existing_local_task {
        return Ok(true);
    }

    with_storage_lock(|| {
        let mut file = read_record_path(&path)?.unwrap_or_else(|| create_file(device_mid));
        if has_identity_record(&file, &user_id) {
            return Ok(false);
        }
        let decision = Decision {
            user_id: Some(user_id),
            status: DecisionStatus::ExistingLocalUser,
            reason: DecisionReason::ExistingLocalTask,
            decided_at: now_rfc3339(),
        };
        upsert_decision(&mut file, decision);
        write_record_path(&path, &file)?;
        Ok(false)
    })
}

fn dismiss_onboarding(app: &AppHandle, device_mid: &str) -> Result<(), String> {
    let user_id = local_anonymous_id(app)?;
    with_storage_lock(|| {
        let path = record_path(app)?;
        let mut file = read_record_path(&path)?.unwrap_or_else(|| create_file(device_mid));
        if file
            .entries
            .iter()
            .any(|entry| entry.user_id.as_deref() == Some(user_id.as_str()))
        {
            return Ok(());
        }
        if file.decisions.iter().any(|decision| {
            decision.user_id.as_deref() == Some(user_id.as_str())
                && decision.status == DecisionStatus::Dismissed
                && decision.reason == DecisionReason::UserClosed
        }) {
            return Ok(());
        }
        upsert_decision(
            &mut file,
            Decision {
                user_id: Some(user_id),
                status: DecisionStatus::Dismissed,
                reason: DecisionReason::UserClosed,
                decided_at: now_rfc3339(),
            },
        );
        write_record_path(&path, &file)
    })
}

fn get_latest_entry(app: &AppHandle) -> Result<Option<Value>, String> {
    get_latest_entry_model(app)?.map(to_value).transpose()
}

fn sync_settings_from_record(app: &AppHandle) -> Result<Option<Value>, String> {
    let Some(entry) = get_latest_entry_model(app)? else {
        return Ok(None);
    };
    let occupation = entry
        .occupation
        .as_deref()
        .filter(|value| is_supported_occupation(value))
        .unwrap_or("other");
    Ok(Some(json!({
        "onboardingOccupation": occupation,
        "proactiveSuggestionsEnabled": entry.proactive_suggestions_enabled.unwrap_or(false),
        "memoryEnabled": entry.memory_enabled.unwrap_or(false),
    })))
}

fn update_record_preferences(app: &AppHandle, patch: &Value) -> Result<(), String> {
    let object = patch
        .as_object()
        .ok_or_else(|| "偏好补丁必须是对象".to_owned())?;
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "memoryEnabled" | "proactiveSuggestionsEnabled"
        ) {
            return Err(format!("偏好补丁字段不受支持：{key}"));
        }
        if !object[key].is_null() && !object[key].is_boolean() {
            return Err(format!("偏好补丁字段 {key} 必须是布尔值或 null"));
        }
    }
    let user_id = local_anonymous_id(app)?;
    with_storage_lock(|| {
        let path = record_path(app)?;
        let Some(mut file) = read_record_path(&path)? else {
            return Ok(());
        };
        let Some(entry) = file
            .entries
            .iter_mut()
            .rev()
            .find(|entry| entry.user_id.as_deref() == Some(user_id.as_str()))
        else {
            return Ok(());
        };
        let before = entry.clone();
        if let Some(value) = object.get("memoryEnabled") {
            entry.memory_enabled = optional_bool_value(value, "memoryEnabled")?;
        }
        if let Some(value) = object.get("proactiveSuggestionsEnabled") {
            entry.proactive_suggestions_enabled =
                optional_bool_value(value, "proactiveSuggestionsEnabled")?;
        }
        if *entry != before {
            write_record_path(&path, &file)?;
        }
        Ok(())
    })
}

fn get_records(app: &AppHandle) -> Result<Option<Value>, String> {
    read_record_path(&record_path(app)?)?
        .map(to_value)
        .transpose()
}

fn clear_records(app: &AppHandle) -> Result<(), String> {
    with_storage_lock(|| {
        let path = record_path(app)?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("删除本地引导记录失败：{error}")),
        }
    })
}

fn get_latest_entry_model(app: &AppHandle) -> Result<Option<RecordEntry>, String> {
    let user_id = local_anonymous_id(app)?;
    let Some(file) = read_record_path(&record_path(app)?)? else {
        return Ok(None);
    };
    Ok(file
        .entries
        .into_iter()
        .rev()
        .find(|entry| entry.user_id.as_deref() == Some(user_id.as_str())))
}

fn create_file(device_mid: &str) -> RecordFile {
    RecordFile {
        version: 2,
        device_mid: device_mid.to_owned(),
        entries: Vec::new(),
        decisions: Vec::new(),
    }
}

fn upsert_entry(file: &mut RecordFile, entry: RecordEntry) -> bool {
    let identity = entry.user_id.clone();
    let previous = file
        .entries
        .iter()
        .find(|existing| existing.user_id == identity)
        .cloned();
    if previous.as_ref() == Some(&entry) {
        return false;
    }
    let mut first_index = None;
    let mut index = 0;
    file.entries.retain(|existing| {
        if existing.user_id == identity {
            if first_index.is_none() {
                first_index = Some(index);
            }
            false
        } else {
            index += 1;
            true
        }
    });
    let insert_at = first_index.unwrap_or(file.entries.len());
    file.entries.insert(insert_at, entry);
    true
}

fn upsert_decision(file: &mut RecordFile, decision: Decision) {
    if let Some(index) = file
        .decisions
        .iter()
        .position(|existing| existing.user_id == decision.user_id)
    {
        file.decisions[index] = decision;
    } else {
        file.decisions.push(decision);
    }
}

fn has_identity_record(file: &RecordFile, user_id: &str) -> bool {
    file.entries
        .iter()
        .any(|entry| entry.user_id.as_deref() == Some(user_id))
        || file
            .decisions
            .iter()
            .any(|decision| decision.user_id.as_deref() == Some(user_id))
}

fn has_local_identity_record(file: &RecordFile, user_id: &str) -> bool {
    has_identity_record(file, user_id)
        || file.entries.iter().any(|entry| entry.user_id.is_none())
        || file
            .decisions
            .iter()
            .any(|decision| decision.user_id.is_none())
}

fn record_path(app: &AppHandle) -> Result<PathBuf, String> {
    crate::storage::root_dir(app)
        .map(|root| root.join(RECORD_FILE))
        .map_err(|error| error.to_string())
}

fn anonymous_id_path(app: &AppHandle) -> Result<PathBuf, String> {
    crate::storage::root_dir(app)
        .map(|root| root.join(ANONYMOUS_ID_FILE))
        .map_err(|error| error.to_string())
}

fn local_anonymous_id(app: &AppHandle) -> Result<String, String> {
    with_storage_lock(|| {
        let path = anonymous_id_path(app)?;
        let Some(bytes) =
            crate::storage::read_private_bytes_bounded(&path, 256, "本地匿名引导身份")
                .map_err(|error| error.to_string())?
        else {
            let mut random = [0_u8; 16];
            getrandom::fill(&mut random)
                .map_err(|error| format!("生成本地匿名身份失败：{error}"))?;
            let id = format!(
                "{LOCAL_ANONYMOUS_PREFIX}{}",
                random
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            crate::storage::atomic_write_private(&path, id.as_bytes())
                .map_err(|error| format!("保存本地匿名身份失败：{error}"))?;
            return Ok(id);
        };
        let id = String::from_utf8(bytes)
            .map_err(|_| "本地匿名引导身份不是有效 UTF-8".to_owned())?
            .trim()
            .to_owned();
        validate_local_anonymous_id(&id)?;
        Ok(id)
    })
}

fn validate_local_anonymous_id(id: &str) -> Result<(), String> {
    let suffix = id
        .strip_prefix(LOCAL_ANONYMOUS_PREFIX)
        .ok_or_else(|| "本地匿名引导身份格式无效".to_owned())?;
    if suffix.len() != 32 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("本地匿名引导身份格式无效".to_owned());
    }
    Ok(())
}

fn read_record_path(path: &Path) -> Result<Option<RecordFile>, String> {
    let Some(bytes) =
        crate::storage::read_private_bytes_bounded(path, MAX_RECORD_BYTES, "引导记录")
            .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    parse_record_bytes(&bytes)
}

fn write_record_path(path: &Path, file: &RecordFile) -> Result<(), String> {
    let bytes =
        serde_json::to_vec_pretty(file).map_err(|error| format!("编码引导记录失败：{error}"))?;
    crate::storage::atomic_write_private(path, &bytes)
        .map_err(|error| format!("保存引导记录失败：{error}"))
}

fn parse_record_bytes(bytes: &[u8]) -> Result<Option<RecordFile>, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| format!("引导记录不是有效 JSON：{error}"))?;
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "引导记录缺少有效 version".to_owned())?;
    let file = match version {
        1 => {
            let file: RecordFileV1 = serde_json::from_value(value)
                .map_err(|error| format!("引导记录 v1 结构无效：{error}"))?;
            if file.version != 1 {
                return Err("引导记录 version 不受支持".to_owned());
            }
            RecordFile {
                version: 2,
                device_mid: file.device_mid,
                entries: file.entries,
                decisions: Vec::new(),
            }
        }
        2 => {
            let file: RecordFileV2 = serde_json::from_value(value)
                .map_err(|error| format!("引导记录 v2 结构无效：{error}"))?;
            if file.version != 2 {
                return Err("引导记录 version 不受支持".to_owned());
            }
            RecordFile {
                version: 2,
                device_mid: file.device_mid,
                entries: file.entries,
                decisions: file.decisions,
            }
        }
        _ => return Err(format!("引导记录 version {version} 不受支持")),
    };
    validate_record(&file)?;
    Ok(Some(file))
}

fn validate_record(file: &RecordFile) -> Result<(), String> {
    if file.version != 2 || file.device_mid.trim().is_empty() {
        return Err("引导记录版本或 deviceMid 无效".to_owned());
    }
    for entry in &file.entries {
        if entry.user_id.as_deref().is_some_and(str::is_empty)
            || entry.occupation.as_deref().is_some_and(str::is_empty)
            || entry.completed_at.trim().is_empty()
        {
            return Err("引导 entry 包含空身份、职业或完成时间".to_owned());
        }
    }
    for decision in &file.decisions {
        if decision.user_id.as_deref().is_some_and(str::is_empty)
            || decision.decided_at.trim().is_empty()
        {
            return Err("引导 decision 包含空身份或决定时间".to_owned());
        }
    }
    Ok(())
}

fn parse_entry_input(value: &Value) -> Result<EntryInput, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "参数 entry 必须是对象".to_owned())?;
    const FIELDS: &[&str] = &[
        "occupation",
        "interfaceMode",
        "memoryEnabled",
        "proactiveSuggestionsEnabled",
        "completedAt",
    ];
    for key in object.keys() {
        if !FIELDS.contains(&key.as_str()) {
            return Err(format!("entry 字段不受支持：{key}"));
        }
    }
    let occupation = optional_string_value(
        object
            .get("occupation")
            .ok_or_else(|| "entry 缺少 occupation".to_owned())?,
        "occupation",
    )?;
    let interface_mode = match object
        .get("interfaceMode")
        .ok_or_else(|| "entry 缺少 interfaceMode".to_owned())?
    {
        Value::Null => None,
        Value::String(value) => match value.as_str() {
            "coding" => Some(InterfaceMode::Coding),
            "office" => Some(InterfaceMode::Office),
            _ => return Err("interfaceMode 必须是 coding、office 或 null".to_owned()),
        },
        _ => return Err("interfaceMode 必须是字符串或 null".to_owned()),
    };
    let memory_enabled = optional_bool_value(
        object
            .get("memoryEnabled")
            .ok_or_else(|| "entry 缺少 memoryEnabled".to_owned())?,
        "memoryEnabled",
    )?;
    let proactive_suggestions_enabled = optional_bool_value(
        object
            .get("proactiveSuggestionsEnabled")
            .ok_or_else(|| "entry 缺少 proactiveSuggestionsEnabled".to_owned())?,
        "proactiveSuggestionsEnabled",
    )?;
    let completed_at = object
        .get("completedAt")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| "completedAt 必须是非空字符串".to_owned())?;
    Ok(EntryInput {
        occupation,
        interface_mode,
        memory_enabled,
        proactive_suggestions_enabled,
        completed_at,
    })
}

fn optional_string_value(value: &Value, name: &str) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) if !value.trim().is_empty() => Ok(Some(value.clone())),
        _ => Err(format!("{name} 必须是非空字符串或 null")),
    }
}

fn optional_bool_value(value: &Value, name: &str) -> Result<Option<bool>, String> {
    match value {
        Value::Null => Ok(None),
        Value::Bool(value) => Ok(Some(*value)),
        _ => Err(format!("{name} 必须是布尔值或 null")),
    }
}

fn required_string(args: &Value, name: &str) -> Result<String, String> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("缺少有效参数 {name}"))
}

fn reject_unknown(args: &Value, allowed: &[&str], method: &str) -> Result<(), String> {
    let object = args
        .as_object()
        .ok_or_else(|| format!("{method} 参数必须是对象"))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("{method} 不支持参数 {key}"));
    }
    Ok(())
}

fn to_value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| format!("引导记录响应编码失败：{error}"))
}

fn with_storage_lock<T>(task: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let lock = STORAGE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().map_err(|_| "引导记录存储锁已损坏".to_owned())?;
    task()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn is_supported_occupation(value: &str) -> bool {
    matches!(
        value,
        "office"
            | "developer"
            | "independent"
            | "infrastructure"
            | "product"
            | "design"
            | "student"
            | "creator"
            | "operations"
            | "marketing"
            | "finance"
            | "accounting"
            | "legal"
            | "other"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn entry(user_id: Option<&str>, completed_at: &str) -> RecordEntry {
        RecordEntry {
            user_id: user_id.map(ToOwned::to_owned),
            occupation: Some("developer".to_owned()),
            interface_mode: Some(InterfaceMode::Coding),
            memory_enabled: Some(true),
            proactive_suggestions_enabled: Some(false),
            completed_at: completed_at.to_owned(),
            upload_state: UploadState::Pending,
        }
    }

    #[test]
    fn parses_v1_as_current_v2_without_losing_entries() {
        let value = serde_json::json!({
            "version": 1,
            "deviceMid": "device-a",
            "entries": [serde_json::to_value(entry(None, "2026-01-01T00:00:00Z")).unwrap()]
        });
        let parsed = parse_record_bytes(&serde_json::to_vec(&value).unwrap())
            .expect("v1 should parse")
            .expect("v1 should be present");
        assert_eq!(parsed.version, 2);
        assert!(parsed.decisions.is_empty());
        assert_eq!(parsed.entries.len(), 1);
    }

    #[test]
    fn append_upsert_is_idempotent_and_replaces_same_identity() {
        let mut file = create_file("device-a");
        assert!(upsert_entry(
            &mut file,
            entry(Some("local-anonymous-a"), "one")
        ));
        let first = file.clone();
        assert!(!upsert_entry(
            &mut file,
            entry(Some("local-anonymous-a"), "one")
        ));
        assert_eq!(file, first);
        assert!(upsert_entry(
            &mut file,
            entry(Some("local-anonymous-a"), "two")
        ));
        assert_eq!(file.entries.len(), 1);
        assert_eq!(file.entries[0].completed_at, "two");
    }

    #[test]
    fn record_write_is_private_and_round_trips_current_schema() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let path = directory.path().join(RECORD_FILE);
        let mut file = create_file("device-a");
        file.entries.push(entry(Some("local-anonymous-a"), "now"));
        write_record_path(&path, &file).expect("写入记录");
        let decoded = read_record_path(&path)
            .expect("读取记录")
            .expect("记录存在");
        assert_eq!(decoded, file);
        assert!(fs::metadata(path).expect("记录元数据").is_file());
    }

    #[test]
    fn source_entry_contract_rejects_unknown_or_missing_fields() {
        let valid = serde_json::json!({
            "occupation": null,
            "interfaceMode": "coding",
            "memoryEnabled": null,
            "proactiveSuggestionsEnabled": false,
            "completedAt": "2026-01-01T00:00:00Z"
        });
        assert!(parse_entry_input(&valid).is_ok());
        assert!(parse_entry_input(&json!({"occupation": null})).is_err());
        assert!(
            parse_entry_input(&json!({
                "occupation": null,
                "interfaceMode": "coding",
                "memoryEnabled": null,
                "proactiveSuggestionsEnabled": false,
                "completedAt": "now",
                "userId": "cloud-account"
            }))
            .is_err()
        );
    }

    #[test]
    fn decision_status_keeps_source_wire_name() {
        let decision = Decision {
            user_id: Some("local-anonymous-0123456789abcdef0123456789abcdef".to_owned()),
            status: DecisionStatus::ExistingLocalUser,
            reason: DecisionReason::ExistingLocalTask,
            decided_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        let value = serde_json::to_value(decision).expect("decision 编码");
        assert_eq!(value["status"], "existing_local_user");
        assert_eq!(value["reason"], "existing_local_task");
    }

    #[test]
    fn local_anonymous_id_is_strictly_local_format() {
        assert!(
            validate_local_anonymous_id("local-anonymous-0123456789abcdef0123456789abcdef").is_ok()
        );
        assert!(validate_local_anonymous_id("user@example.com").is_err());
        assert!(validate_local_anonymous_id("local-anonymous-short").is_err());
    }
}
