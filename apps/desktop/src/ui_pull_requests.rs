//! 原 PR 页面的数据适配。只读请求交给本机 gh，固定 GitHub 主机与命令；固定操作只保存本地展示偏好。
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::AppHandle;

const MAX_OUTPUT: u64 = 8 * 1024 * 1024;
const PAGE_LIMIT: usize = 100;
static PIN_LOCK: Mutex<()> = Mutex::new(());
const LIST_FIELDS: &str = "number,title,url,author,headRefName,baseRefName,state,isDraft,additions,deletions,createdAt,updatedAt,reviewDecision,reviewRequests,labels,mergeable";
const DETAIL_FIELDS: &str = "number,title,url,author,headRefName,baseRefName,state,isDraft,additions,deletions,createdAt,updatedAt,reviewDecision,reviewRequests,labels,mergeable,body,changedFiles,mergedAt,closedAt,maintainerCanModify,mergeStateStatus,statusCheckRollup";

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QueryInput {
    project_id: Option<String>,
    repository: Option<String>,
    number: Option<u64>,
    state: Option<String>,
    involvement: Option<String>,
    #[serde(default)]
    force_refresh: bool,
    is_pinned: Option<bool>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Pin {
    project_id: String,
    repository: String,
    number: u64,
}

/// 仅接受 GitHub 的标准 HTTPS/SSH 远程，不接受凭据、选项、任意主机或路径穿越。
fn repository_from_remote(remote: &str) -> Result<String, String> {
    let remote = remote.trim();
    if remote.contains("/../") || remote.contains("/./") || remote.chars().any(char::is_control) {
        return Err("GitHub 远程路径无效".into());
    }
    let path = if let Some(path) = remote.strip_prefix("git@github.com:") {
        path.to_owned()
    } else {
        let url = url::Url::parse(remote).map_err(|_| "项目 origin 不是 GitHub 远程")?;
        if url.host_str() != Some("github.com")
            || url.port().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.scheme(), "https" | "ssh")
            || url.password().is_some()
            || (url.scheme() == "https" && !url.username().is_empty())
            || (url.scheme() == "ssh" && url.username() != "git")
        {
            return Err("项目 origin 不是标准 GitHub 远程".into());
        }
        url.path().trim_start_matches('/').to_owned()
    };
    let path = path.strip_suffix(".git").unwrap_or(&path);
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.starts_with(['-', '.'])
                || part.len() > 100
                || !part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
    {
        return Err("GitHub 仓库标识无效".into());
    }
    Ok(path.to_owned())
}

/// 排空管道但只保留上限内的输出；超时终止 gh，禁止交互提示及控制台窗口。
fn gh(args: &[&str]) -> Result<std::process::Output, String> {
    let mut command = Command::new("gh");
    command
        .args(args)
        .env("GH_HOST", "github.com")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "gh-not-installed".to_owned()
        } else {
            format!("无法启动 GitHub CLI：{error}")
        }
    })?;
    fn drain(
        mut pipe: impl Read + Send + 'static,
    ) -> std::thread::JoinHandle<Result<Vec<u8>, String>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.by_ref()
                .take(MAX_OUTPUT + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            std::io::copy(&mut pipe, &mut std::io::sink()).map_err(|e| e.to_string())?;
            if bytes.len() as u64 > MAX_OUTPUT {
                Err("GitHub 响应超过大小上限".into())
            } else {
                Ok(bytes)
            }
        })
    }
    let stdout = drain(child.stdout.take().ok_or("gh stdout 不可用")?);
    let stderr = drain(child.stderr.take().ok_or("gh stderr 不可用")?);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(match result {
                    Err(e) => format!("GitHub 进程等待失败：{e}"),
                    _ => "GitHub 查询超时，已终止".into(),
                });
            }
        }
    };
    let stdout = stdout.join().map_err(|_| "GitHub stdout 读取任务失败")??;
    let stderr = stderr.join().map_err(|_| "GitHub stderr 读取任务失败")??;
    Ok(std::process::Output {
        status: status?,
        stdout,
        stderr,
    })
}

fn text(args: &[&str]) -> Result<String, String> {
    let output = gh(args)?;
    if !output.status.success() {
        let error = keencode_model::redact_error_secrets(&String::from_utf8_lossy(&output.stderr));
        return Err(if error.trim().is_empty() {
            "GitHub 查询失败".into()
        } else {
            error
        });
    }
    String::from_utf8(output.stdout).map_err(|_| "GitHub 响应不是 UTF-8".into())
}
fn data(args: &[&str]) -> Result<Value, String> {
    serde_json::from_str(&text(args)?).map_err(|e| format!("GitHub JSON 响应无效：{e}"))
}

fn unavailable(reason: &str) -> Value {
    json!({"unavailable":{"_tag":"PullRequestsUnavailableError", "reason":reason, "message":
        if reason == "gh-not-installed" {"未安装 GitHub CLI，请安装 gh 后重试"} else {"GitHub CLI 未登录，请运行 gh auth login 后重试"}}})
}
fn is_auth_missing(message: &str) -> bool {
    message.contains("gh auth login") || message.contains("not logged into")
}
/// 显式 GH_HOST 会允许 gh api 发起匿名请求；先验登录状态，避免把未登录误报为限流。
fn authenticated_viewer(
    read: &impl Fn(&[&str]) -> Result<String, String>,
) -> Result<String, String> {
    read(&["auth", "status", "--hostname", "github.com"])?;
    read(&[
        "api",
        "--hostname",
        "github.com",
        "--method",
        "GET",
        "user",
        "--jq",
        ".login",
    ])
}
fn actor(value: &Value) -> Value {
    let login = value.get("login").or_else(|| value.get("slug"));
    if login.and_then(Value::as_str).is_none_or(str::is_empty) {
        return Value::Null;
    }
    json!({"login":login,"name":value["name"], "avatarUrl":value.get("avatarUrl").or_else(||value.get("avatar_url")),
        "url":value.get("html_url").or_else(||value.get("url"))})
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn mergeability(value: &Value) -> &'static str {
    match value.as_str() {
        Some("MERGEABLE") => "mergeable",
        Some("CONFLICTING") => "conflicting",
        _ => "unknown",
    }
}
fn entry(
    project: &crate::workspace::ProjectRecord,
    repository: &str,
    raw: &Value,
    viewer: &str,
    pins: &[Pin],
) -> Value {
    json!({"projectId":project.id,"projectTitle":project.name,"repository":repository,
        "number":raw["number"],"title":raw["title"],"url":raw["url"],"author":actor(&raw["author"]),
        "headBranch":raw["headRefName"],"baseBranch":raw["baseRefName"],
        "state":raw["state"].as_str().unwrap_or("").to_ascii_lowercase(),"isDraft":raw["isDraft"],
        "additions":raw["additions"],"deletions":raw["deletions"],"createdAt":raw["createdAt"],"updatedAt":raw["updatedAt"],
        "reviewDecision":raw["reviewDecision"].as_str().filter(|s|!s.is_empty()),
        "viewerReviewRequested":array(&raw["reviewRequests"]).iter().any(|actor| actor["login"]==viewer),
        "labels":array(&raw["labels"]).iter().map(|v|json!({"name":v["name"],"color":v["color"]})).collect::<Vec<_>>(),
        "isPinned":pins.iter().any(|p|p.project_id==project.id && p.repository.eq_ignore_ascii_case(repository) && raw["number"]==p.number),
        "mergeability":mergeability(&raw["mergeable"]),"stack":null})
}

/// 评论查询独立失败不遮蔽 PR 正文；达到三页上限时保留截断标记，不能伪称完整。
fn pages(
    repository: &str,
    suffix: &str,
    cap: usize,
    read: &impl Fn(&[&str]) -> Result<Value, String>,
) -> Result<(Vec<Value>, bool), String> {
    let mut entries = Vec::new();
    for page in 1..=cap {
        let path = format!("repos/{repository}/{suffix}?per_page=100&page={page}");
        let values = read(&["api", "--hostname", "github.com", "--method", "GET", &path])?;
        let values = values.as_array().ok_or("GitHub 分页响应格式无效")?;
        entries.extend(values.iter().cloned());
        if values.len() < PAGE_LIMIT {
            return Ok((entries, false));
        }
    }
    Ok((entries, true))
}
fn check(raw: &Value) -> Value {
    let state = raw["conclusion"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| raw["state"].as_str())
        .unwrap_or("PENDING");
    let status = match state {
        "SUCCESS" => "success",
        "FAILURE" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" => "failure",
        "SKIPPED" => "skipped",
        "NEUTRAL" => "neutral",
        "CANCELLED" | "STALE" => "cancelled",
        _ => "pending",
    };
    json!({"name":raw.get("name").or_else(||raw.get("context")), "status":status,"description":raw["description"],
        "url":raw.get("detailsUrl").or_else(||raw.get("targetUrl")),"startedAt":raw["startedAt"],"completedAt":raw["completedAt"]})
}
fn detail(
    project: &crate::workspace::ProjectRecord,
    repository: &str,
    number: u64,
    viewer: &str,
    pins: &[Pin],
    read: &impl Fn(&[&str]) -> Result<Value, String>,
) -> Result<Value, String> {
    let raw = read(&[
        "pr",
        "view",
        &number.to_string(),
        "--repo",
        repository,
        "--json",
        DETAIL_FIELDS,
    ])?;
    let mut result = entry(project, repository, &raw, viewer, pins);
    let mut comments = Vec::new();
    let mut incomplete = false;
    let mut truncated = false;
    for (suffix, kind) in [
        (format!("issues/{number}/comments"), "issue-comment"),
        (format!("pulls/{number}/comments"), "review-comment"),
        (format!("pulls/{number}/reviews"), "review"),
    ] {
        match pages(repository, &suffix, 3, read) {
            Ok((values, capped)) => {
                truncated |= capped;
                for value in values {
                    let created = value
                        .get("created_at")
                        .or_else(|| value.get("submitted_at"));
                    // 尚未提交的 review 没有时间，不能混入原页面已发布的时间线。
                    if created.is_none_or(Value::is_null) {
                        continue;
                    }
                    comments.push(json!({"id":format!("{kind}:{}",value["id"]),"kind":kind,"author":actor(&value["user"]),"body":value["body"].as_str().unwrap_or(""),
                        "createdAt":created,"updatedAt":value["updated_at"],"url":value["html_url"],"path":value["path"],"reviewState":value["state"]}));
                }
            }
            Err(_) => incomplete = true,
        }
    }
    comments.sort_by(|a, b| a["createdAt"].as_str().cmp(&b["createdAt"].as_str()));
    let (commits, commits_truncated) =
        pages(repository, &format!("pulls/{number}/commits"), 10, read)?;
    if commits_truncated {
        return Err("PR 提交列表超过查询上限，无法返回完整时间线".into());
    }
    let repo = read(&[
        "api",
        "--hostname",
        "github.com",
        "--method",
        "GET",
        &format!("repos/{repository}"),
    ])?;
    let extra = json!({"workspaceRoot":project.path,"body":raw["body"].as_str().unwrap_or(""),"mergeable":raw["mergeable"],
        "mergeStateStatus":raw["mergeStateStatus"],"changedFiles":raw["changedFiles"],"mergedAt":raw["mergedAt"],"closedAt":raw["closedAt"],
        "maintainerCanModify":raw["maintainerCanModify"],"reviewers":array(&raw["reviewRequests"]).iter().filter_map(|v|{let a=actor(v);(!a.is_null()).then_some(a)}).collect::<Vec<_>>(),
        "checks":array(&raw["statusCheckRollup"]).iter().map(check).collect::<Vec<_>>(),"comments":comments,"commentsIncomplete":incomplete,"commentsTruncated":truncated,
        "commits":commits.iter().map(|v|{let message=v["commit"]["message"].as_str().unwrap_or(""); let (headline,body)=message.split_once('\n').unwrap_or((message,""));
            let author=&v["author"]; json!({"oid":v["sha"],"messageHeadline":headline,"messageBody":body.trim(),"committedDate":v["commit"]["committer"]["date"],
                "authors":[{"login":author["login"],"name":v["commit"]["author"]["name"],"avatarUrl":author["avatar_url"],"url":author["html_url"]}]})}).collect::<Vec<_>>(),
        "mergeCapabilities":{"merge":repo["allow_merge_commit"],"squash":repo["allow_squash_merge"],"rebase":repo["allow_rebase_merge"],"deleteBranchOnMerge":repo["delete_branch_on_merge"]},
        // gh 的常规 PR 查询没有 stack 契约；保持未知，禁止原页面把它当作已确认的单独合并。
        "stackMetadataIncomplete":true});
    result
        .as_object_mut()
        .ok_or("PR 投影无效")?
        .extend(extra.as_object().ok_or("PR 详情投影无效")?.clone());
    Ok(result)
}

fn load_pins(root: &Path) -> Result<Vec<Pin>, String> {
    let bytes = crate::storage::read_private_bytes_bounded(
        &root.join("ui-pull-request-pins.json"),
        1024 * 1024,
        "PR 固定偏好",
    )
    .map_err(|e| e.to_string())?;
    let pins: Vec<Pin> = bytes
        .map(|b| serde_json::from_slice(&b).map_err(|e| format!("PR 固定偏好格式无效：{e}")))
        .transpose()?
        .unwrap_or_default();
    if pins.len() > 2048
        || pins.iter().any(|p| {
            p.project_id.is_empty()
                || p.project_id.len() > 256
                || p.number == 0
                || repository_from_remote(&format!("https://github.com/{}", p.repository)).is_err()
        })
    {
        return Err("PR 固定偏好内容无效".into());
    }
    Ok(pins)
}
fn set_pin(root: &Path, pin: Pin, pinned: bool) -> Result<(), String> {
    let _guard = PIN_LOCK.lock().map_err(|_| "PR 固定偏好锁已损坏")?;
    let mut pins = load_pins(root)?;
    pins.retain(|p| {
        !(p.project_id == pin.project_id
            && p.repository.eq_ignore_ascii_case(&pin.repository)
            && p.number == pin.number)
    });
    if pinned {
        pins.push(pin);
    }
    if pins.len() > 2048 {
        return Err("PR 固定数量超过上限".into());
    }
    crate::storage::atomic_write_private(
        &root.join("ui-pull-request-pins.json"),
        &serde_json::to_vec(&pins).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn query(app: &AppHandle, operation: &str, input: QueryInput) -> Result<Value, String> {
    if !["list", "review-count", "detail", "diff", "pin"].contains(&operation) {
        return Err("未知 PR 查询操作".into());
    }
    let state = input.state.as_deref().unwrap_or("open");
    let involvement = input.involvement.as_deref().unwrap_or("all");
    if !["open", "closed", "merged"].contains(&state)
        || !["all", "authored", "reviewing"].contains(&involvement)
    {
        return Err("PR 查询筛选无效".into());
    }
    // 页面 forceRefresh 自带重查语义；宿主没有额外轮询或跨用户的远程缓存。
    let _force_refresh = input.force_refresh;
    let mut projects = crate::workspace::project_records(app)?;
    if let Some(id) = input.project_id.as_deref() {
        projects.retain(|p| p.id == id);
        if projects.is_empty() {
            return Err("PR 项目尚未添加".into());
        }
    }
    let root = crate::storage::root_dir(app).map_err(|e| e.to_string())?;
    if operation == "pin" {
        let project = projects
            .first()
            .filter(|_| input.project_id.is_some())
            .ok_or("缺少 PR 项目")?;
        let repository =
            crate::ui_git::git(Path::new(&project.path), &["remote", "get-url", "origin"])
                .and_then(|s| repository_from_remote(&s))?;
        if input
            .repository
            .as_deref()
            .is_none_or(|r| !r.eq_ignore_ascii_case(&repository))
        {
            return Err("PR 仓库与项目 origin 不一致".into());
        }
        let number = input.number.filter(|n| *n > 0).ok_or("PR 编号无效")?;
        let pinned = input.is_pinned.ok_or("缺少 PR 固定状态")?;
        set_pin(
            &root,
            Pin {
                project_id: project.id.clone(),
                repository: repository.clone(),
                number,
            },
            pinned,
        )?;
        return Ok(
            json!({"projectId":project.id,"repository":repository,"number":number,"isPinned":pinned}),
        );
    }
    let viewer = match authenticated_viewer(&text) {
        Ok(viewer) => viewer.trim().to_owned(),
        Err(error) if error == "gh-not-installed" => return Ok(unavailable("gh-not-installed")),
        Err(error) if is_auth_missing(&error) => return Ok(unavailable("gh-not-authenticated")),
        Err(error) => return Err(error),
    };
    if viewer.is_empty() {
        return Err("GitHub 当前用户响应为空".into());
    }
    let pins = load_pins(&root)?;
    let mut errors = Vec::new();
    let mut grouped: BTreeMap<String, Vec<crate::workspace::ProjectRecord>> = BTreeMap::new();
    for project in projects {
        // 非 Git 项目是原首页聊天目录，直接略过；Git 项目的无效 origin 保留真实错误。
        if crate::ui_git::git(Path::new(&project.path), &["rev-parse", "--git-dir"]).is_err() {
            continue;
        }
        match crate::ui_git::git(Path::new(&project.path), &["remote", "get-url", "origin"])
            .and_then(|s| repository_from_remote(&s))
        {
            Ok(repo) => grouped
                .entry(repo.to_ascii_lowercase())
                .or_default()
                .push(project),
            Err(error) => errors
                .push(json!({"projectId":project.id,"projectTitle":project.name,"message":error})),
        }
    }
    if matches!(operation, "detail" | "diff") {
        let requested = input.repository.as_deref().ok_or("缺少 PR 仓库")?;
        let group = grouped
            .get(&requested.to_ascii_lowercase())
            .ok_or("PR 仓库与项目 origin 不一致")?;
        let project = group
            .first()
            .filter(|_| input.project_id.is_some())
            .ok_or("缺少 PR 项目")?;
        let number = input.number.filter(|n| *n > 0).ok_or("PR 编号无效")?;
        if operation == "detail" {
            return detail(project, requested, number, &viewer, &pins, &data);
        }
        let patch = text(&[
            "pr",
            "diff",
            &number.to_string(),
            "--repo",
            requested,
            "--color",
            "never",
        ])?;
        return Ok(json!({"patch":patch,"truncated":false}));
    }
    let mut entries = Vec::new();
    let mut batches = Vec::new();
    let mut incomplete = false;
    for (repo, group) in grouped {
        let project = &group[0];
        let mut args = vec![
            "pr",
            "list",
            "--repo",
            &repo,
            "--state",
            if operation == "review-count" {
                "open"
            } else {
                state
            },
            "--limit",
            "100",
            "--json",
            LIST_FIELDS,
        ];
        let mut filters = Vec::new();
        if operation != "review-count" && state == "closed" {
            filters.push("is:unmerged".to_owned());
        }
        if operation == "review-count" || involvement == "reviewing" {
            filters.push(format!("review-requested:{viewer}"));
        } else if involvement == "authored" {
            args.extend(["--author", &viewer]);
        }
        let search = filters.join(" ");
        if !search.is_empty() {
            args.extend(["--search", &search]);
        }
        match data(&args) {
            Ok(raw) => {
                let values = raw.as_array().ok_or("GitHub PR 列表响应格式无效")?;
                incomplete |= values.len() >= PAGE_LIMIT;
                batches.push(json!({"projectId":project.id,"projectTitle":project.name,"repository":repo,"truncated":values.len()>=PAGE_LIMIT}));
                for raw in values {
                    let mut row = entry(project, &repo, raw, &viewer, &pins);
                    row["projectContexts"]=json!(group.iter().map(|p|json!({"projectId":p.id,"projectTitle":p.name,"isPinned":pins.iter().any(|pin|pin.project_id==p.id && pin.repository.eq_ignore_ascii_case(&repo) && raw["number"]==pin.number)})).collect::<Vec<_>>());
                    entries.push(row);
                }
            }
            Err(message) => errors.push(
                json!({"projectId":project.id,"projectTitle":project.name,"message":message}),
            ),
        }
    }
    if operation == "review-count" {
        return Ok(json!({"count":entries.len(),"incomplete":incomplete||!errors.is_empty()}));
    }
    Ok(json!({"viewer":viewer,"entries":entries,"errors":errors,"repositoryBatches":batches}))
}

#[tauri::command]
pub async fn ui_pull_requests(
    app: AppHandle,
    operation: String,
    input: QueryInput,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || query(&app, &operation, input))
        .await
        .map_err(|e| format!("PR 查询后台任务失败：{e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_identity_rejects_host_credentials_options_and_traversal() {
        for remote in [
            "https://github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo",
        ] {
            assert_eq!(repository_from_remote(remote).unwrap(), "owner/repo");
        }
        for remote in [
            "https://token@github.com/owner/repo",
            "https://github.com.evil/owner/repo",
            "git@github.com:-owner/repo",
            "https://github.com/owner/../repo",
            "https://github.com/owner/repo?token=x",
            "file:///tmp/repo",
            "ssh://git@github.com:22/owner/repo",
        ] {
            assert!(repository_from_remote(remote).is_err(), "{remote}");
        }
    }
    #[test]
    fn github_mapping_preserves_actual_state_and_review_request() {
        let project = crate::workspace::ProjectRecord {
            id: "project-a".into(),
            name: "fixture".into(),
            path: "D:/fixture".into(),
            path_ok: Some(true),
        };
        let raw = json!({"number":17,"state":"MERGED","author":{"login":"author","avatarUrl":"https://github.com/a.png"},"reviewRequests":[{"login":"viewer"}],"mergeable":"UNKNOWN","reviewDecision":"","labels":[{"name":"fix","color":"abcdef"}]});
        let row = entry(&project, "owner/repo", &raw, "viewer", &[]);
        assert_eq!(row["state"], "merged");
        assert_eq!(row["viewerReviewRequested"], true);
        assert_eq!(row["mergeability"], "unknown");
        assert!(row["reviewDecision"].is_null());
        assert_eq!(row["author"]["avatarUrl"], "https://github.com/a.png");
        assert_eq!(
            check(&json!({"name":"CI","status":"IN_PROGRESS","conclusion":""}))["status"],
            "pending"
        );
        assert_eq!(
            check(&json!({"context":"lint","state":"ERROR"}))["status"],
            "failure"
        );
        assert!(!is_auth_missing("HTTP 403 API rate limit exceeded"));
        assert!(is_auth_missing("please run gh auth login"));
    }
    #[test]
    fn pins_are_durable_idempotent_and_project_scoped() {
        let root = tempfile::tempdir().unwrap();
        let pin = Pin {
            project_id: "project-a".into(),
            repository: "owner/repo".into(),
            number: 17,
        };
        set_pin(root.path(), pin.clone(), true).unwrap();
        set_pin(root.path(), pin.clone(), true).unwrap();
        assert!(load_pins(root.path()).unwrap() == vec![pin.clone()]);
        let other = Pin {
            project_id: "project-b".into(),
            ..pin.clone()
        };
        set_pin(root.path(), other.clone(), true).unwrap();
        set_pin(root.path(), pin, false).unwrap();
        assert!(load_pins(root.path()).unwrap() == vec![other]);
    }

    #[test]
    fn detail_maps_comments_checks_commits_and_incomplete_metadata() {
        let project = crate::workspace::ProjectRecord {
            id: "p".into(),
            name: "fixture".into(),
            path: "D:/fixture".into(),
            path_ok: Some(true),
        };
        let read = |args: &[&str]| -> Result<Value, String> {
            if args[0] == "pr" {
                return Ok(
                    json!({"number":17,"state":"OPEN","body":"原正文","changedFiles":2,"mergeable":"CONFLICTING","statusCheckRollup":[{"context":"lint","state":"SUCCESS"}],"reviewRequests":[{"slug":"core","name":"Core team"}]}),
                );
            }
            let path = args.last().unwrap();
            if path.contains("issues/17/comments") {
                return Ok(
                    json!([{"id":1,"body":"普通评论","created_at":"2026-10-01T00:00:00Z","user":{"login":"author","avatar_url":"https://github.com/a.png"}}]),
                );
            }
            if path.contains("pulls/17/comments") {
                return Err("评论 API 暂时失败".into());
            }
            if path.contains("pulls/17/reviews") {
                return Ok(
                    json!([{"id":2,"body":"审阅","submitted_at":"2026-10-01T01:00:00Z","state":"APPROVED","user":{"login":"reviewer"}},{"id":3,"submitted_at":null}]),
                );
            }
            if path.contains("pulls/17/commits") {
                return Ok(
                    json!([{"sha":"abc","author":null,"commit":{"message":"中文提交\n\n正文","author":{"name":"Deleted user"},"committer":{"date":"2026-10-01T02:00:00Z"}}}]),
                );
            }
            Ok(
                json!({"allow_merge_commit":true,"allow_squash_merge":false,"allow_rebase_merge":true,"delete_branch_on_merge":false}),
            )
        };
        let result = detail(&project, "owner/repo", 17, "viewer", &[], &read).unwrap();
        assert_eq!(result["body"], "原正文");
        assert_eq!(result["mergeability"], "conflicting");
        assert_eq!(result["reviewers"][0]["login"], "core");
        assert_eq!(result["checks"][0]["status"], "success");
        assert_eq!(result["comments"].as_array().unwrap().len(), 2);
        assert_eq!(result["comments"][1]["kind"], "review");
        assert_eq!(result["commentsIncomplete"], true);
        assert_eq!(result["commentsTruncated"], false);
        assert_eq!(result["commits"][0]["messageHeadline"], "中文提交");
        assert_eq!(result["commits"][0]["messageBody"], "正文");
        assert!(result["commits"][0]["authors"][0]["login"].is_null());
        assert_eq!(result["mergeCapabilities"]["squash"], false);
        assert_eq!(result["stackMetadataIncomplete"], true);
    }

    #[test]
    fn pagination_reports_caps_and_retains_page_failures() {
        let calls = std::cell::RefCell::new(Vec::new());
        let read = |args: &[&str]| {
            calls.borrow_mut().push(args.last().unwrap().to_string());
            Ok(json!((0..100).collect::<Vec<_>>()))
        };
        let (entries, truncated) = pages("owner/repo", "issues/17/comments", 3, &read).unwrap();
        assert_eq!(entries.len(), 300);
        assert!(truncated);
        assert!(calls.borrow()[2].ends_with("page=3"));
        let failure = |_: &[&str]| Err("HTTP 403".into());
        assert_eq!(
            pages("owner/repo", "pulls/17/commits", 10, &failure).unwrap_err(),
            "HTTP 403"
        );
    }

    #[test]
    fn authentication_precedes_api_call_and_retains_network_failure() {
        let calls = std::cell::RefCell::new(Vec::new());
        let missing = |args: &[&str]| {
            calls.borrow_mut().push(args[0].to_owned());
            Err("You are not logged into any GitHub hosts. To log in, run: gh auth login".into())
        };
        assert!(is_auth_missing(
            &authenticated_viewer(&missing).unwrap_err()
        ));
        assert_eq!(*calls.borrow(), vec!["auth"]);
        let limited = |args: &[&str]| {
            if args[0] == "auth" {
                Ok("signed in".into())
            } else {
                Err("HTTP 403 API rate limit exceeded".into())
            }
        };
        assert_eq!(
            authenticated_viewer(&limited).unwrap_err(),
            "HTTP 403 API rate limit exceeded"
        );
        assert!(!is_auth_missing(
            &authenticated_viewer(&limited).unwrap_err()
        ));
    }
}
