//! 原 Profile 页的本地统计数据适配；权威输入仍为 Session Journal 和请求记录。
//! 生命周期投影只保留计数所需的身份与时间，不保存消息正文或供应商凭据。
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use keencode_resources::{JournalConfig, SessionEvent, SessionJournal, SessionOpen};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex},
};
use tauri::{AppHandle, Manager};

const ARCHIVE_FILE: &str = "profile-activity.json";
const ARCHIVE_SCHEMA: &str = "keencode/profile-activity";
const MAX_BYTES: u64 = 64 * 1024 * 1024;
static IO_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivityArchive {
    schema: String,
    version: u32,
    threads: BTreeSet<String>,
    /// 消息和输入回执的原始全局身份在分叉时不变，复制历史不会重复计数。
    prompts: BTreeMap<String, PromptFact>,
    turns: BTreeMap<String, TurnFact>,
    skills: BTreeMap<String, SkillFact>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PromptFact {
    time_ms: u64,
    turn_id: String,
    session_id: String,
    project_id: String,
    title: String,
    root: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TurnFact {
    model: String,
    reasoning: Option<String>,
}
impl TurnFact {
    /// 投影内部保留供应商身份；原页面的 model 字段只显示模型名，统一归入 KeenCode。
    fn model_name(&self) -> &str {
        self.model
            .split_once("::")
            .map_or(self.model.as_str(), |(_, model)| model)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SkillFact {
    name: String,
    display_name: String,
    kind: String,
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn load(root: &Path) -> Result<ActivityArchive, String> {
    let bytes = crate::storage::read_private_bytes_bounded(
        &root.join(ARCHIVE_FILE),
        MAX_BYTES,
        "Profile 统计",
    )
    .map_err(error)?;
    match bytes {
        None => Ok(ActivityArchive {
            schema: ARCHIVE_SCHEMA.into(),
            version: 1,
            ..Default::default()
        }),
        Some(bytes) => {
            let state: ActivityArchive = serde_json::from_slice(&bytes).map_err(error)?;
            if state.schema != ARCHIVE_SCHEMA || state.version != 1 {
                return Err("Profile 统计格式无效".into());
            }
            Ok(state)
        }
    }
}

/// 读取当前持久事务类型，排除编辑产生的内部历史副本；用户分叉仍是独立线程。
#[derive(Default)]
struct MutationOrigins {
    archives: BTreeSet<String>,
    copied_prefixes: BTreeMap<String, (String, u64)>,
}
impl MutationOrigins {
    fn owner(&self, session: &str, sequence: u64) -> Result<String, String> {
        let mut owner = session;
        let mut visited = BTreeSet::new();
        while let Some((source, through)) = self.copied_prefixes.get(owner) {
            if sequence > *through {
                break;
            }
            if !visited.insert(owner) {
                return Err("会话复制来源存在循环".into());
            }
            owner = source;
        }
        Ok(owner.into())
    }
}
fn mutation_origins(directory: &Path) -> Result<MutationOrigins, String> {
    let path = directory.join("session-mutations/records");
    if !path.try_exists().map_err(error)? {
        return Ok(MutationOrigins::default());
    }
    let metadata = std::fs::symlink_metadata(&path).map_err(error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("会话事务目录无效".into());
    }
    let mut origins = MutationOrigins::default();
    for entry in std::fs::read_dir(path).map_err(error)? {
        let entry = entry.map_err(error)?;
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let bytes = crate::storage::read_private_bytes_bounded(&path, 1024 * 1024, "会话事务")
            .map_err(error)?
            .ok_or("会话事务消失")?;
        let record: Value = serde_json::from_slice(&bytes).map_err(error)?;
        if record["schema"] != "keencode/session-mutation"
            || !record["version"]
                .as_u64()
                .is_some_and(|version| version > 0)
        {
            return Err("会话事务格式无效".into());
        }
        let target = record["targetSessionId"]
            .as_str()
            .ok_or("会话事务目标无效")?;
        let source = record["sourceSessionId"]
            .as_str()
            .ok_or("会话事务来源无效")?;
        keencode_resources::SessionId::new(target.to_owned()).map_err(error)?;
        keencode_resources::SessionId::new(source.to_owned()).map_err(error)?;
        let through = record["targetLastSequence"]
            .as_u64()
            .ok_or("会话复制前缀无效")?;
        origins
            .copied_prefixes
            .insert(target.into(), (source.into(), through));
        if record["kind"]["type"] == "edit_user" {
            origins.archives.insert(target.into());
        }
    }
    Ok(origins)
}

struct SessionProjection<'a> {
    archive: &'a mut ActivityArchive,
    root_turns: BTreeSet<String>,
    session_id: String,
    project: keencode_resources::ProjectStorage,
    skill_requests: BTreeMap<String, String>,
}
impl SessionProjection<'_> {
    fn prompt(
        &mut self,
        id: String,
        time_ms: u64,
        turn: &str,
        refs: &[keencode_model::InputReference],
    ) {
        self.archive
            .prompts
            .entry(id.clone())
            .or_insert_with(|| PromptFact {
                time_ms,
                turn_id: turn.into(),
                session_id: self.session_id.clone(),
                project_id: self.project.id.clone(),
                title: self.project.name.clone(),
                root: self.project.path.clone(),
            });
        for reference in refs {
            if let Some(identity) = reference.path.strip_prefix("plugin://") {
                self.archive
                    .skills
                    .entry(format!("{id}:plugin:{identity}"))
                    .or_insert_with(|| SkillFact {
                        name: identity.into(),
                        display_name: reference.name.clone(),
                        kind: "skill".into(),
                    });
            }
        }
    }
    fn observe(&mut self, event: &SessionEvent, event_id: &str, time_ms: u64) {
        match event {
            SessionEvent::AtomicBatch { events } => {
                for event in events {
                    self.observe(event, event_id, time_ms);
                }
            }
            SessionEvent::TurnStarted {
                turn_id,
                source_agent_id,
                parent_turn_id: None,
                ..
            } if source_agent_id.as_str() == "root" => {
                self.root_turns.insert(turn_id.as_str().into());
            }
            SessionEvent::TurnProviderSnapshotRecorded {
                turn_id, provider, ..
            } => {
                self.archive
                    .turns
                    .entry(turn_id.as_str().into())
                    .or_insert_with(|| TurnFact {
                        model: format!("{}::{}", provider.provider_id, provider.model),
                        reasoning: provider
                            .reasoning_effort
                            .and_then(|value| serde_json::to_value(value).ok())
                            .and_then(|value| value.as_str().map(str::to_owned)),
                    });
            }
            SessionEvent::MessageAdded { message }
                if message.role == keencode_resources::MessageRole::User
                    && !message.is_meta
                    && message.agent_id.is_none() =>
            {
                if let Some(turn) = &message.turn_id
                    && self.root_turns.contains(turn.as_str())
                {
                    self.prompt(
                        format!("message:{}", message.message_id),
                        time_ms,
                        turn.as_str(),
                        &message.references,
                    );
                }
            }
            SessionEvent::DynamicInputReceiptCommitted {
                turn_id,
                source_agent_id,
                kind: keencode_resources::DynamicInputKind::UserSteer,
                user_inputs,
                ..
            } if source_agent_id.as_str() == "root" => {
                for input in user_inputs {
                    self.prompt(
                        format!("steer:{event_id}:{}", input.sequence),
                        time_ms,
                        turn_id.as_str(),
                        &input.references,
                    );
                }
            }
            SessionEvent::ToolRequested { request } if request.tool_name == "Skill" => {
                if let Some(name) = request.arguments.get("name").and_then(Value::as_str) {
                    self.skill_requests
                        .insert(request.request_id.as_str().into(), name.into());
                }
            }
            SessionEvent::ToolCompleted {
                request_id,
                outcome,
            } if outcome.status == keencode_resources::ToolCompletionStatus::Succeeded
                && !outcome.result.is_error =>
            {
                if let Some(name) = self.skill_requests.get(request_id.as_str()) {
                    self.archive
                        .skills
                        .entry(format!("skill:{event_id}"))
                        .or_insert_with(|| SkillFact {
                            name: name.clone(),
                            display_name: name.clone(),
                            kind: "skill".into(),
                        });
                }
            }
            _ => {}
        }
    }
}

fn refresh(root: &Path) -> Result<ActivityArchive, String> {
    let _guard = IO_LOCK.lock().map_err(|_| "Profile 统计锁不可用")?;
    let mut archive = load(root)?;
    // 按需扫描经过资源层校验的项目数据目录，不访问用户源码或创建常驻统计线程。
    for directory in keencode_resources::project_storage_directories(root).map_err(error)? {
        let bytes = crate::storage::read_private_bytes_bounded(
            &directory.join("project.json"),
            64 * 1024,
            "项目统计元数据",
        )
        .map_err(error)?
        .ok_or("项目统计元数据缺失")?;
        let project: keencode_resources::ProjectStorage =
            serde_json::from_slice(&bytes).map_err(error)?;
        let origins = mutation_origins(&directory)?;
        for id in keencode_resources::list_session_ids(&directory).map_err(error)? {
            // 资源层验证完整日志、CAS、schema 和原子事件；损坏数据不能伪装成零活动。
            let journal =
                match SessionJournal::open(&directory, id.clone(), JournalConfig::default())
                    .map_err(error)?
                {
                    SessionOpen::Ready(journal) => journal,
                    SessionOpen::Corrupt(_) => return Err("Profile 无法统计损坏的会话日志".into()),
                };
            if !origins.archives.contains(id.as_str()) {
                archive.threads.insert(id.as_str().into());
            }
            let mut projection = SessionProjection {
                archive: &mut archive,
                root_turns: BTreeSet::new(),
                session_id: id.as_str().into(),
                project: project.clone(),
                skill_requests: BTreeMap::new(),
            };
            let mut after = None;
            loop {
                let page = journal.read_page(after, 1000).map_err(error)?;
                for record in page.records {
                    projection.session_id = origins.owner(id.as_str(), record.sequence)?;
                    projection.observe(
                        &record.event,
                        record.event_id.as_str(),
                        record.time_unix_ms,
                    );
                }
                if !page.has_more {
                    break;
                }
                after = page.next_after;
            }
        }
    }
    let bytes = serde_json::to_vec(&archive).map_err(error)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Profile 统计超过容量限制".into());
    }
    crate::storage::atomic_write_private(&root.join(ARCHIVE_FILE), &bytes).map_err(error)?;
    Ok(archive)
}

/// 删除历史前保留生命周期投影，失败则不进入实际删除。无消息正文进入统计归档。
pub(crate) fn preserve_before_delete(root: &Path) -> Result<(), String> {
    refresh(root).map(|_| ())
}

/// 原页面传入东向 UTC 偏移，固定偏移只用于本次查询，UTC 时间事实不被改写。
fn validate_offset(offset: i32) -> Result<(), String> {
    if !(-1440..=1440).contains(&offset) {
        return Err("Profile 时区偏移越界".into());
    }
    Ok(())
}
fn local_time(ms: u64, offset: i32) -> Result<DateTime<Utc>, String> {
    let ms = i64::try_from(ms).map_err(error)?;
    DateTime::from_timestamp_millis(ms)
        .and_then(|time| time.checked_add_signed(Duration::minutes(i64::from(offset))))
        .ok_or("Profile 活动时间无效".into())
}
fn percent(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64 * 1000.0).round() / 10.0
    }
}
fn heatmap(days: &BTreeMap<NaiveDate, u64>, today: NaiveDate) -> Vec<Value> {
    let start = today - Duration::days(273);
    let counts: Vec<_> = days
        .range(start..=today)
        .map(|(_, count)| *count)
        .filter(|count| *count > 0)
        .collect();
    (0..274).map(|index| {
        let day = start + Duration::days(index);
        let count = days.get(&day).copied().unwrap_or(0);
        let rank = counts.iter().filter(|value| **value <= count).count();
        let intensity = if count == 0 || counts.is_empty() { 0 } else { (rank * 4).div_ceil(counts.len()).clamp(1, 4) };
        json!({ "day": day.to_string(), "count": count, "weekday": day.weekday().num_days_from_sunday(), "intensity": intensity })
    }).collect()
}
fn streaks(days: &BTreeMap<NaiveDate, u64>, today: NaiveDate) -> (u64, u64) {
    let active: BTreeSet<_> = days
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(day, _)| *day)
        .collect();
    let mut longest = 0;
    let mut run = 0;
    let mut previous = None;
    for day in &active {
        run = if previous == Some(*day - Duration::days(1)) {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
        previous = Some(*day);
    }
    let mut day = if active.contains(&today) {
        today
    } else {
        today - Duration::days(1)
    };
    let mut current = 0;
    while active.contains(&day) {
        current += 1;
        day -= Duration::days(1);
    }
    (current, longest)
}

/// 原页面最多取前六项；接口沿用来源的前八项，按真实用量排序，比例仍以全部模型为分母。
fn ranked_model_counts(counts: &BTreeMap<String, u64>) -> Vec<(&String, &u64)> {
    let mut entries: Vec<_> = counts.iter().filter(|(_, count)| **count > 0).collect();
    entries.sort_by(|(left_model, left_count), (right_model, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_model.cmp(right_model))
    });
    entries.truncate(8);
    entries
}

fn profile(
    archive: &ActivityArchive,
    offset: i32,
    now: DateTime<Utc>,
    home: &str,
) -> Result<Value, String> {
    let today = (now + Duration::minutes(i64::from(offset))).date_naive();
    let mut days = BTreeMap::new();
    let mut hours = [0u64; 24];
    let mut models = BTreeMap::<String, u64>::new();
    let mut reasoning = BTreeMap::<String, u64>::new();
    let mut turns = BTreeSet::new();
    let mut projects = BTreeMap::<String, Vec<&PromptFact>>::new();
    for prompt in archive.prompts.values() {
        let time = local_time(prompt.time_ms, offset)?;
        *days.entry(time.date_naive()).or_default() += 1;
        hours[time.hour() as usize] += 1;
        projects
            .entry(prompt.project_id.clone())
            .or_default()
            .push(prompt);
        if turns.insert(&prompt.turn_id)
            && let Some(turn) = archive.turns.get(&prompt.turn_id)
        {
            *models.entry(turn.model_name().to_owned()).or_default() += 1;
            if let Some(value) = &turn.reasoning {
                *reasoning.entry(value.clone()).or_default() += 1;
            }
        }
    }
    let count = archive.prompts.len() as u64;
    let turn_count = models.values().sum::<u64>();
    let (current, longest) = streaks(&days, today);
    let peak_hour = hours
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .max_by(|(a, ac), (b, bc)| ac.cmp(bc).then_with(|| b.cmp(a)));
    let top_reasoning = reasoning
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)));
    let mut skill_counts = BTreeMap::<(&str, &str), (&str, u64)>::new();
    for skill in archive.skills.values() {
        let entry = skill_counts
            .entry((&skill.kind, &skill.name))
            .or_insert((&skill.display_name, 0));
        entry.1 += 1;
    }
    let mut skills: Vec<_> = skill_counts.iter().map(|((kind, name), (display, runs))| json!({"name": name, "displayName": display, "kind": kind, "runCount": runs})).collect();
    skills.sort_by(|a, b| {
        b["runCount"]
            .as_u64()
            .cmp(&a["runCount"].as_u64())
            .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
    });
    let most_project = projects.iter().max_by(|a, b| a.1.len().cmp(&b.1.len()).then_with(|| b.0.cmp(a.0))).map(|(id, prompts)| {
        let last = prompts.iter().max_by_key(|prompt| prompt.time_ms).expect("项目至少一个提示");
        let project_days: BTreeSet<_> = prompts.iter().filter_map(|p| local_time(p.time_ms, offset).ok()).map(|time| time.date_naive()).collect();
        let threads: BTreeSet<_> = prompts.iter().map(|p| &p.session_id).collect();
        json!({"projectId": id, "title": last.title, "workspaceRoot": last.root, "promptCount": prompts.len(), "threadCount": threads.len(), "activeDays": project_days.len(), "lastWorkedAt": DateTime::from_timestamp_millis(last.time_ms as i64).expect("提示时间已校验").to_rfc3339()})
    });
    let initials: String = home
        .split([' ', '.', '_', '-'])
        .filter_map(|part| part.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    let handle: String = home
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(30)
        .collect();
    Ok(json!({
        "generatedAt": now.to_rfc3339(), "timezone": {"utcOffsetMinutes": offset, "today": today.to_string()},
        "identity": {"homeDirBasename": home, "initials": initials, "defaultHandle": format!("@{}", if handle.is_empty() { "keencode" } else { &handle })},
        "activity": {"currentStreakDays": current, "longestStreakDays": longest, "totalPromptsSent": count, "totalThreads": archive.threads.len(), "promptsToday": days.get(&today).copied().unwrap_or(0), "heatmapMetric": "prompts", "heatmap": heatmap(&days, today)},
        "activeHours": {"startHour": peak_hour.map(|(hour, _)| hour), "endHour": peak_hour.map(|(hour, _)| (hour + 1) % 24), "turnCount": peak_hour.map(|(_, count)| *count).unwrap_or(0), "label": null},
        "insights": {"topProvider": if turn_count > 0 { Some("keencode") } else { None }, "topProviderPercent": if turn_count > 0 { Some(100) } else { None }, "topReasoning": top_reasoning.map(|(name, _)| name), "topReasoningPercent": top_reasoning.map(|(_, count)| percent(*count, turn_count)), "skillsExplored": skills.len(), "totalSkillsUsed": archive.skills.len()},
        "providerModels": ranked_model_counts(&models).into_iter().map(|(model, count)| json!({"provider": "keencode", "model": model, "turnCount": count, "percent": percent(*count, turn_count)})).collect::<Vec<_>>(),
        "mostUsedSkill": skills.first(), "skills": skills, "mostWorkedProject": most_project,
        "quota": {"status": "unavailable", "provider": null, "window": null, "usedPercent": null, "resetsAt": null, "planName": null}
    }))
}

fn tokens(
    records: &[crate::analytics::RequestRecord],
    archive: &ActivityArchive,
    offset: i32,
    now: DateTime<Utc>,
) -> Result<Value, String> {
    let mut days = BTreeMap::<NaiveDate, u64>::new();
    let mut models = BTreeMap::<String, u64>::new();
    let mut total = 0u64;
    let mut available = false;
    let mut missing = false;
    for record in records
        .iter()
        .filter(|record| record.session_id.is_some() && record.status == "success")
    {
        if !record.usage_reported {
            missing = true;
            continue;
        }
        available = true;
        let count = record.input_tokens.saturating_add(record.output_tokens);
        let day = local_time(record.requested_at_ms, offset)?.date_naive();
        let amount = days.entry(day).or_default();
        *amount = amount.saturating_add(count);
        let model = record
            .turn_id
            .as_ref()
            .and_then(|id| archive.turns.get(id))
            .map(TurnFact::model_name)
            .filter(|model| *model == record.model)
            .unwrap_or(record.model.as_str())
            .to_owned();
        let amount = models.entry(model).or_default();
        *amount = amount.saturating_add(count);
        total = total.saturating_add(count);
    }
    let peak = days
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)));
    let today = (now + Duration::minutes(i64::from(offset))).date_naive();
    Ok(
        json!({"available": available, "lifetimeTotalTokens": if available { Some(total) } else { None }, "peakDayTokens": peak.map(|(_, count)| count), "peakDay": peak.map(|(day, _)| day.to_string()),
        "providers": if available { vec!["keencode"] } else { vec![] }, "unavailableProviders": if missing { vec!["keencode"] } else { vec![] },
        "topProvider": if available && total > 0 { Some("keencode") } else { None }, "topProviderPercent": if available && total > 0 { Some(100) } else { None },
        "models": ranked_model_counts(&models).into_iter().map(|(model, count)| json!({"provider": "keencode", "model": model, "tokens": count, "percent": percent(*count, total)})).collect::<Vec<_>>(), "heatmapMetric": "tokens", "heatmap": heatmap(&days, today)}),
    )
}

#[tauri::command]
pub async fn ui_profile_stats(app: AppHandle, utc_offset_minutes: i32) -> Result<Value, String> {
    validate_offset(utc_offset_minutes)?;
    let root = crate::storage::root_dir(&app).map_err(error)?;
    let home = app
        .path()
        .home_dir()
        .map_err(error)?
        .file_name()
        .ok_or("用户主目录名称无效")?
        .to_string_lossy()
        .into_owned();
    tauri::async_runtime::spawn_blocking(move || {
        profile(&refresh(&root)?, utc_offset_minutes, Utc::now(), &home)
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub async fn ui_profile_token_stats(
    app: AppHandle,
    recorder: tauri::State<'_, Arc<crate::analytics::AnalyticsRecorder>>,
    utc_offset_minutes: i32,
) -> Result<Value, String> {
    validate_offset(utc_offset_minutes)?;
    let root = crate::storage::root_dir(&app).map_err(error)?;
    let recorder = recorder.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        recorder.flush()?;
        tokens(
            &crate::analytics::read_records(&app)?,
            &refresh(&root)?,
            utc_offset_minutes,
            Utc::now(),
        )
    })
    .await
    .map_err(error)?
}

#[cfg(test)]
mod tests;
