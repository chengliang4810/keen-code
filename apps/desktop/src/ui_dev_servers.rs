//! 原 Start dev 的数据接口：项目拥有进程树，界面只投影宿主登记和退出事件。
use command_group::{AsyncCommandGroup, AsyncGroupChild};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path, process::Stdio, sync::Arc, time::Duration};
use tauri::{AppHandle, Emitter, State};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{mpsc, oneshot},
};

type StopReply = oneshot::Sender<Result<(), String>>;
type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    project_id: String,
    command: String,
    cwd: String,
    pid: u32,
    started_at: String,
    status: &'static str,
    /// 仅保留脚本声明的loopback HTTP origin；实际端口仍以OS监听表核对。
    #[serde(skip)]
    announced: Arc<Mutex<HashMap<u16, String>>>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Event {
    Upserted {
        server: Server,
    },
    Removed {
        #[serde(rename = "projectId")]
        project_id: String,
        reason: &'static str,
    },
}

struct Entry {
    server: Server,
    stop: mpsc::UnboundedSender<StopReply>,
}

#[derive(Default)]
pub struct DevServers {
    entries: Mutex<HashMap<String, Entry>>,
    // 与启动串行化，退出清理先关闭入口，禁止清理期间再产生未登记进程。
    closing: Mutex<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunInput {
    project_id: String,
    command: String,
    cwd: String,
    env: Option<HashMap<String, String>>,
}

fn error(context: &str, cause: impl std::fmt::Display) -> String {
    keencode_model::redact_error_secrets(&format!("{context}：{cause}"))
}

/// 目录按登记项目ID定位；子项目可运行，符号链接解析后不得跳出项目根目录。
fn run_directory(root: &Path, cwd: &str) -> Result<std::path::PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|e| error("项目目录不可访问", e))?;
    let cwd = Path::new(cwd)
        .canonicalize()
        .map_err(|e| error("脚本工作目录不可访问", e))?;
    if !cwd.is_dir() || !cwd.starts_with(root) {
        return Err("脚本工作目录不属于此项目".into());
    }
    // cmd.exe 不接受扩展盘符路径；只在系统命令边界转回普通盘符/UNC。
    #[cfg(windows)]
    {
        let text = cwd.to_string_lossy();
        if let Some(path) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(format!(r"\\{path}").into());
        }
        if let Some(path) = text.strip_prefix(r"\\?\") {
            return Ok(path.into());
        }
    }
    Ok(cwd)
}

fn spawn(input: &RunInput) -> Result<AsyncGroupChild, String> {
    if input.command.trim().is_empty()
        || input.command.len() > 32768
        || input.command.contains('\0')
    {
        return Err("开发脚本命令为空或超过大小限制".into());
    }
    crate::terminal::validate_environment(input.env.as_ref())?;
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd.exe");
        // 关闭AutoRun；整条用户脚本只由一次系统shell解析，不改写包管理器语义。
        command.args(["/D", "/S", "/C", &input.command]);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &input.command]);
        command
    };
    command
        .current_dir(&input.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(env) = &input.env {
        command.envs(env);
    }
    keencode_tools::apply_to_tokio_command(&mut command);
    let mut group = command.group();
    group.kill_on_drop(true);
    #[cfg(windows)]
    group.creation_flags(0x0800_0000);
    group.spawn().map_err(|e| error("无法启动开发脚本", e))
}

/// 输出持续排空但不落盘、不广播脚本环境或正文，避免大输出堵塞服务与泄露凭据。
async fn drain(mut stream: impl AsyncRead + Unpin, announced: Arc<Mutex<HashMap<u16, String>>>) {
    let mut bytes = [0; 8192];
    let mut tail = Vec::new();
    while let Ok(n) = stream.read(&mut bytes).await {
        if n == 0 {
            break;
        }
        tail.extend_from_slice(&bytes[..n]);
        for url in declared_urls(&String::from_utf8_lossy(&tail)) {
            if let Some(port) = url.port_or_known_default() {
                let mut announced = announced.lock();
                if announced.len() < 64 || announced.contains_key(&port) {
                    announced.insert(port, url.origin().ascii_serialization());
                }
            }
        }
        // 只为跨批次URL保留尾部；不保存或返回完整进程输出。
        if tail.len() > 2048 {
            tail.drain(..tail.len() - 2048);
        }
    }
}

fn declared_urls(text: &str) -> Vec<url::Url> {
    static PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(https?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]|\[::\]):[0-9]{1,5})[/\s\x1b]",
        )
        .expect("固定URL模式")
    });
    PATTERN
        .captures_iter(text)
        .filter_map(|m| {
            let mut url = url::Url::parse(m.get(1)?.as_str()).ok()?;
            if url.port_or_known_default()? == 0 {
                return None;
            }
            if matches!(url.host_str(), Some("0.0.0.0" | "[::]")) {
                url.set_host(Some("localhost")).ok()?;
            }
            Some(url)
        })
        .collect()
}

async fn terminate(child: &mut AsyncGroupChild, root_pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    let handles =
        tauri::async_runtime::spawn_blocking(move || process_completion::capture(root_pid))
            .await
            .map_err(|e| error("开发脚本进程句柄查询失败", e))??;
    #[cfg(not(windows))]
    let _ = root_pid;
    if let Err(e) = child.start_kill()
        && !matches!(
            e.kind(),
            std::io::ErrorKind::InvalidInput | std::io::ErrorKind::NotFound
        )
        && !(cfg!(unix) && e.raw_os_error() == Some(3))
    {
        return Err(error("停止开发脚本进程树失败", e));
    }
    child
        .wait()
        .await
        .map_err(|e| error("回收开发脚本失败", e))?;
    #[cfg(windows)]
    tauri::async_runtime::spawn_blocking(move || process_completion::wait(handles))
        .await
        .map_err(|e| error("等待开发脚本后代退出失败", e))??;
    Ok(())
}

impl DevServers {
    fn list(&self) -> Vec<Server> {
        let mut values: Vec<_> = self
            .entries
            .lock()
            .values()
            .map(|entry| entry.server.clone())
            .collect();
        values.sort_by(|a, b| a.project_id.cmp(&b.project_id));
        values
    }

    fn start(self: &Arc<Self>, input: RunInput, events: EventSink) -> Result<Server, String> {
        let closing = self.closing.lock();
        if *closing {
            return Err("应用正在退出，不能启动开发脚本".into());
        }
        let mut entries = self.entries.lock();
        if entries.contains_key(&input.project_id) {
            return Err("此项目已有运行中的开发脚本".into());
        }
        if entries.len() >= 16 {
            return Err("同时运行的项目脚本超过上限".into());
        }
        let mut child = spawn(&input)?;
        let server = Server {
            project_id: input.project_id,
            command: input.command,
            cwd: input.cwd,
            pid: child.id().ok_or("开发脚本未返回进程标识")?,
            started_at: chrono::Utc::now().to_rfc3339(),
            status: "running",
            announced: Arc::default(),
        };
        let stdout = child
            .inner()
            .stdout
            .take()
            .map(|pipe| tauri::async_runtime::spawn(drain(pipe, server.announced.clone())));
        let stderr = child
            .inner()
            .stderr
            .take()
            .map(|pipe| tauri::async_runtime::spawn(drain(pipe, server.announced.clone())));
        let (stop, mut stopped) = mpsc::unbounded_channel::<StopReply>();
        entries.insert(
            server.project_id.clone(),
            Entry {
                server: server.clone(),
                stop,
            },
        );
        events(Event::Upserted {
            server: server.clone(),
        });
        let manager = Arc::downgrade(self);
        let id = server.project_id.clone();
        let root_pid = server.pid;
        tauri::async_runtime::spawn(async move {
            let (reason, reply) = loop {
                tokio::select! {
                    status = child.inner().wait() => {
                        if let Err(e) = status { tracing::error!(error = %error("开发脚本等待失败", e)); }
                        // 主shell退出时也收割可能遗留的后代，禁止脚本脱离登记生命周期。
                        if let Err(e) = terminate(&mut child, root_pid).await { tracing::error!(error = %e); }
                        break ("exited", None);
                    }
                    request = stopped.recv() => {
                        let result = terminate(&mut child, root_pid).await;
                        if result.is_ok() { break ("stopped", request); }
                        if let Some(reply) = request { let _ = reply.send(result); }
                        else { let _ = child.start_kill(); break ("stopped", None); }
                    }
                }
            };
            if let Some(task) = stdout {
                task.abort();
            }
            if let Some(task) = stderr {
                task.abort();
            }
            if let Some(manager) = manager.upgrade() {
                let mut entries = manager.entries.lock();
                entries.remove(&id);
                // 与下次启动的upsert保持同一登记锁顺序，旧removed不能覆盖新一轮。
                events(Event::Removed {
                    project_id: id,
                    reason,
                });
            }
            if let Some(reply) = reply {
                let _ = reply.send(Ok(()));
            }
        });
        Ok(server)
    }

    async fn stop(&self, id: &str) -> Result<bool, String> {
        self.stop_matching(id, None).await
    }

    async fn stop_matching(&self, id: &str, expected_pid: Option<u32>) -> Result<bool, String> {
        let sender = {
            let entries = self.entries.lock();
            if let Some(entry) = entries.get(id) {
                if expected_pid.is_some_and(|pid| pid != entry.server.pid) {
                    return Err("此项目服务已经重新启动，请刷新后再停止".into());
                }
                Some(entry.stop.clone())
            } else {
                None
            }
        };
        let Some(sender) = sender else {
            return Ok(false);
        };
        let (reply, result) = oneshot::channel();
        // 自然退出与停止可同时发生；仅在确认登记已移除时视为已经停止。
        if sender.send(reply).is_err() {
            return Ok(!self.entries.lock().contains_key(id));
        }
        match tokio::time::timeout(Duration::from_secs(5), result).await {
            Ok(Ok(result)) => result.map(|()| true),
            Ok(Err(_)) if !self.entries.lock().contains_key(id) => Ok(true),
            _ => Err("开发脚本停止尚未完成".into()),
        }
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        *self.closing.lock() = true;
        let ids: Vec<_> = self.entries.lock().keys().cloned().collect();
        let mut first_error = None;
        for id in ids {
            if let Err(e) = self.stop(&id).await {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[tauri::command]
pub async fn ui_dev_server_start(
    app: AppHandle,
    manager: State<'_, Arc<DevServers>>,
    mut input: RunInput,
) -> Result<serde_json::Value, String> {
    let project = crate::workspace::project_records(&app)?
        .into_iter()
        .find(|p| p.id == input.project_id)
        .ok_or("开发脚本项目不存在")?;
    input.cwd = run_directory(Path::new(&project.path), &input.cwd)?
        .to_string_lossy()
        .into_owned();
    let events: EventSink = Arc::new(move |event| {
        let _ = app.emit("project://dev-server", event);
    });
    Ok(serde_json::json!({"server": manager.inner().start(input, events)?}))
}

#[tauri::command]
pub async fn ui_dev_server_stop(
    manager: State<'_, Arc<DevServers>>,
    project_id: String,
) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({"stopped":manager.stop(&project_id).await?}))
}

#[tauri::command]
pub fn ui_dev_server_list(manager: State<'_, Arc<DevServers>>) -> serde_json::Value {
    serde_json::json!({"servers":manager.list()})
}

/// 本机端口按请求枚举；只允许停止此管理器拥有的项目进程树，不按任意PID杀进程。
#[tauri::command]
pub async fn ui_local_servers_list(
    manager: State<'_, Arc<DevServers>>,
) -> Result<serde_json::Value, String> {
    let servers = manager.list();
    tauri::async_runtime::spawn_blocking(move || local_servers::list(&servers))
        .await
        .map_err(|e| error("本机服务查询失败", e))?
}

#[tauri::command]
pub async fn ui_local_server_stop(
    manager: State<'_, Arc<DevServers>>,
    pid: u32,
    port: u16,
) -> Result<serde_json::Value, String> {
    let servers = manager.list();
    let owned = tauri::async_runtime::spawn_blocking(move || {
        let id = local_servers::owner(&servers, pid, port)?;
        let root_pid = servers
            .iter()
            .find(|s| s.project_id == id)
            .ok_or("服务登记已移除")?
            .pid;
        Ok::<_, String>((id, root_pid))
    })
    .await
    .map_err(|e| error("本机服务归属查询失败", e))??;
    Ok(
        serde_json::json!({"pid":pid,"stopped":manager.stop_matching(&owned.0, Some(owned.1)).await?}),
    )
}

mod local_servers;
#[cfg(windows)]
mod process_completion;

#[cfg(test)]
mod tests;
