//! 开发命令进程树管理。

use command_group::{AsyncCommandGroup, AsyncGroupChild};
use parking_lot::Mutex;
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{mpsc, oneshot},
};

use crate::native_paths::NativePaths;

type StopReply = oneshot::Sender<Result<(), String>>;
pub type DevServerEventSink = Arc<dyn Fn(DevServerEvent) + Send + Sync>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevServerSnapshot {
    pub project_id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub pid: u32,
    pub started_at: String,
    pub status: &'static str,
    pub announced_urls: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DevServerEvent {
    Upserted {
        server: DevServerSnapshot,
    },
    Removed {
        project_id: String,
        reason: &'static str,
    },
}

#[derive(Clone, Debug)]
pub struct DevServerInput {
    pub project_id: String,
    pub project_root: PathBuf,
    pub cwd: PathBuf,
    pub command: String,
    pub env: Option<HashMap<String, String>>,
}

struct Entry {
    server: DevServerSnapshot,
    announced: Arc<Mutex<HashMap<u16, String>>>,
    stop: mpsc::UnboundedSender<StopReply>,
}

pub struct NativeDevServers {
    paths: Arc<NativePaths>,
    entries: Mutex<HashMap<String, Entry>>,
    closing: Mutex<bool>,
}

impl NativeDevServers {
    pub fn new(paths: Arc<NativePaths>) -> Self {
        Self {
            paths,
            entries: Mutex::new(HashMap::new()),
            closing: Mutex::new(false),
        }
    }

    pub fn paths(&self) -> &Arc<NativePaths> {
        &self.paths
    }

    pub fn list(&self) -> Vec<DevServerSnapshot> {
        let mut values: Vec<_> = self
            .entries
            .lock()
            .values()
            .map(|entry| {
                let mut server = entry.server.clone();
                server.announced_urls = entry.announced.lock().values().cloned().collect();
                server
            })
            .collect();
        values.sort_by(|left, right| left.project_id.cmp(&right.project_id));
        values
    }

    pub fn start(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        input: DevServerInput,
        events: DevServerEventSink,
    ) -> Result<DevServerSnapshot, String> {
        let root = canonical_directory(&input.project_root)?;
        let cwd = canonical_directory(&input.cwd)?;
        if !cwd.starts_with(&root) {
            return Err("脚本工作目录不属于项目根目录".to_owned());
        }
        validate_command(&input.command)?;
        validate_environment(input.env.as_ref())?;
        let closing = self.closing.lock();
        if *closing {
            return Err("应用正在退出，不能启动开发脚本".to_owned());
        }
        let mut entries = self.entries.lock();
        if entries.contains_key(&input.project_id) {
            return Err("此项目已有运行中的开发脚本".to_owned());
        }
        if entries.len() >= 16 {
            return Err("同时运行的项目脚本超过上限".to_owned());
        }
        let mut child = spawn(&input.command, &cwd, input.env.as_ref())?;
        let pid = child.id().ok_or("开发脚本未返回进程标识")?;
        let announced = Arc::new(Mutex::new(HashMap::new()));
        let server = DevServerSnapshot {
            project_id: input.project_id.clone(),
            command: input.command.clone(),
            cwd,
            pid,
            started_at: chrono::Utc::now().to_rfc3339(),
            status: "running",
            announced_urls: Vec::new(),
        };
        let stdout = child
            .inner()
            .stdout
            .take()
            .map(|pipe| runtime.spawn(drain(pipe, Arc::clone(&announced))));
        let stderr = child
            .inner()
            .stderr
            .take()
            .map(|pipe| runtime.spawn(drain(pipe, Arc::clone(&announced))));
        let (stop, mut stopped) = mpsc::unbounded_channel::<StopReply>();
        entries.insert(
            input.project_id.clone(),
            Entry {
                server: server.clone(),
                announced: Arc::clone(&announced),
                stop,
            },
        );
        events(DevServerEvent::Upserted {
            server: server.clone(),
        });
        let manager = Arc::downgrade(self);
        let id = input.project_id;
        runtime.spawn(async move {
            let (reason, reply) = loop {
                tokio::select! {
                    result = child.inner().wait() => {
                        if let Err(error) = result {
                            tracing::error!(%error, "开发脚本等待失败");
                        }
                        if let Err(error) = terminate(&mut child).await {
                            tracing::error!(%error, "开发脚本进程树回收失败");
                        }
                        break ("exited", None);
                    }
                    request = stopped.recv() => {
                        let result = terminate(&mut child).await;
                        if result.is_ok() {
                            break ("stopped", request);
                        }
                        if let Some(reply) = request {
                            let _ = reply.send(result);
                        } else {
                            let _ = child.start_kill();
                            break ("stopped", None);
                        }
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
                manager.entries.lock().remove(&id);
                events(DevServerEvent::Removed {
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

    pub async fn stop(&self, project_id: &str) -> Result<bool, String> {
        self.stop_matching(project_id, None).await
    }

    pub async fn stop_matching(
        &self,
        project_id: &str,
        expected_pid: Option<u32>,
    ) -> Result<bool, String> {
        let sender = {
            let entries = self.entries.lock();
            let Some(entry) = entries.get(project_id) else {
                return Ok(false);
            };
            if expected_pid.is_some_and(|pid| pid != entry.server.pid) {
                return Err("此项目服务已经重新启动，请刷新后再停止".to_owned());
            }
            entry.stop.clone()
        };
        let (reply, result) = oneshot::channel();
        if sender.send(reply).is_err() {
            return Ok(!self.entries.lock().contains_key(project_id));
        }
        match tokio::time::timeout(Duration::from_secs(5), result).await {
            Ok(Ok(result)) => result.map(|()| true),
            Ok(Err(_)) if !self.entries.lock().contains_key(project_id) => Ok(true),
            _ => Err("开发脚本停止尚未完成".to_owned()),
        }
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        *self.closing.lock() = true;
        let ids: Vec<_> = self.entries.lock().keys().cloned().collect();
        let mut first_error = None;
        for id in ids {
            if let Err(error) = self.stop(&id).await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Default for NativeDevServers {
    fn default() -> Self {
        Self::new(Arc::new(NativePaths::from_data_root(PathBuf::new())))
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    let path = path
        .canonicalize()
        .map_err(|error| format!("脚本目录不可访问：{error}"))?;
    if !path.is_dir() {
        return Err("脚本工作目录不是目录".to_owned());
    }
    Ok(path)
}

fn validate_command(command: &str) -> Result<(), String> {
    if command.trim().is_empty() || command.len() > 32_768 || command.contains('\0') {
        return Err("开发脚本命令为空或超过大小限制".to_owned());
    }
    Ok(())
}

fn validate_environment(env: Option<&HashMap<String, String>>) -> Result<(), String> {
    let Some(env) = env else {
        return Ok(());
    };
    if env.len() > 256
        || env
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 1024 * 1024
    {
        return Err("开发脚本环境变量超过大小限制".to_owned());
    }
    if env
        .iter()
        .any(|(key, value)| key.is_empty() || key.contains(['=', '\0']) || value.contains('\0'))
    {
        return Err("开发脚本环境变量格式无效".to_owned());
    }
    Ok(())
}

fn spawn(
    command_text: &str,
    cwd: &Path,
    env: Option<&HashMap<String, String>>,
) -> Result<AsyncGroupChild, String> {
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/S", "/C", command_text]);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", command_text]);
        command
    };
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(env) = env {
        command.envs(env);
    }
    keencode_tools::apply_to_tokio_command(&mut command);
    let mut group = command.group();
    group.kill_on_drop(true);
    #[cfg(windows)]
    group.creation_flags(0x0800_0000);
    group
        .spawn()
        .map_err(|error| format!("无法启动开发脚本：{error}"))
}

async fn drain(mut stream: impl AsyncRead + Unpin, announced: Arc<Mutex<HashMap<u16, String>>>) {
    let mut bytes = [0; 8192];
    let mut tail = Vec::new();
    while let Ok(size) = stream.read(&mut bytes).await {
        if size == 0 {
            break;
        }
        tail.extend_from_slice(&bytes[..size]);
        for url in declared_urls(&String::from_utf8_lossy(&tail)) {
            if let Some(port) = url.port_or_known_default() {
                let mut values = announced.lock();
                if values.len() < 64 || values.contains_key(&port) {
                    values.insert(port, url.origin().ascii_serialization());
                }
            }
        }
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
        .expect("固定开发服务 URL 模式")
    });
    PATTERN
        .captures_iter(text)
        .filter_map(|capture| {
            let mut url = url::Url::parse(capture.get(1)?.as_str()).ok()?;
            let port = url.port_or_known_default()?;
            if port == 0 {
                return None;
            }
            if matches!(url.host_str(), Some("0.0.0.0" | "[::]")) {
                url.set_host(Some("localhost")).ok()?;
            }
            Some(url)
        })
        .collect()
}

async fn terminate(child: &mut AsyncGroupChild) -> Result<(), String> {
    if let Err(error) = child.start_kill()
        && !matches!(
            error.kind(),
            std::io::ErrorKind::InvalidInput | std::io::ErrorKind::NotFound
        )
    {
        return Err(format!("停止开发脚本进程树失败：{error}"));
    }
    child
        .wait()
        .await
        .map_err(|error| format!("回收开发脚本失败：{error}"))?;
    Ok(())
}
