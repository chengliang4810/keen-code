//! 仅在显式 Windows 原生测试二进制内验证真实 ACP mailbox 交错链路。
//!
//! 外部 Node 夹具负责本地模型服务和只读验证；本宿主只复用正式桌面装配、正式
//! acp_dispatch、真实 Journal 和真实 Tauri 事件循环，不写 Runtime/Coordinator。
//! 运行时需要设置 KEENCODE_NATIVE_MAILBOX_FIXTURE_DIR、KEENCODE_BENCHMARK=1、
//! KEENCODE_BENCHMARK_DATA_DIR 和 WEBVIEW2_USER_DATA_FOLDER，并显式运行 ignored 测试。

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tauri::{AppHandle, Manager};

/// 单次正式 ACP 调用的测试超时。
const ACP_TIMEOUT: Duration = Duration::from_secs(60);
/// 每个人工观察阶段的等待上限。
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(180);
/// 三个独立用户回合的稳定身份和输入。
const TURN_TEXTS: [(&str, &str); 3] = [
    ("native-mailbox-root-1", "KC_MAILBOX_ROOT_FIRST"),
    ("native-mailbox-root-2", "KC_MAILBOX_ROOT_SECOND"),
    ("native-mailbox-root-3", "KC_MAILBOX_ROOT_THIRD"),
];

/// 从环境读取且规范化本次原生夹具根；根目录本身不能是符号链接。
fn fixture_root_from_environment(variable: &str) -> Result<PathBuf, String> {
    let raw = PathBuf::from(
        std::env::var_os(variable).ok_or_else(|| "必须明确指定原生 mailbox 夹具根".to_owned())?,
    );
    if !raw.is_absolute() {
        return Err("原生 mailbox 夹具根必须是绝对路径".to_owned());
    }
    let metadata = fs::symlink_metadata(&raw).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("原生 mailbox 夹具根必须是非符号链接目录".to_owned());
    }
    raw.canonicalize().map_err(|error| error.to_string())
}

/// 校验目录是夹具根的直接子目录，拒绝相对路径和路径穿越。
fn assert_exact_fixture_child(
    path: &Path,
    fixture_root: &Path,
    expected_name: &str,
) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(format!("{expected_name} 必须是无穿越的绝对路径"));
    }
    if path.file_name() != Some(std::ffi::OsStr::new(expected_name)) {
        return Err(format!("隔离目录必须命名为 {expected_name}"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("{expected_name} 缺少父目录"))?
        .canonicalize()
        .map_err(|error| format!("规范化 {expected_name} 父目录失败: {error}"))?;
    if parent != fixture_root {
        return Err(format!("{expected_name} 必须是夹具根的直接子目录"));
    }
    Ok(())
}

/// 检查已有路径为普通目录；测试不替换外部夹具创建的数据。
fn require_directory(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("{label} 必须已由外部夹具创建: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{label} 必须是非符号链接目录"));
    }
    Ok(())
}

/// 拒绝复用已有控制信号或证据，保证每个运行实例独占结果。
fn assert_absent(path: &Path, label: &str) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(format!("{label} 已存在，拒绝复用: {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("检查 {label} 失败: {error}")),
    }
}

/// 使用 create_new 写入 JSON；不覆盖任何已有证据。
pub(super) fn write_json_create_new(path: &Path, value: &Value) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("独占创建 {} 失败: {error}", path.display()))?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("序列化 {} 失败: {error}", path.display()))?;
    file.write_all(&bytes)
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|error| format!("写入 {} 失败: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("同步 {} 失败: {error}", path.display()))
}

/// 使用 create_new 保存生产文件原始字节，不解析或重新编码 Journal。
fn write_bytes_create_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("独占创建原始文件 {} 失败: {error}", path.display()))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("写入原始文件 {} 失败: {error}", path.display()))
}

/// 有界等待外部 Node 夹具创建一个普通文件。
pub(super) fn wait_for_signal(path: &Path, label: &str) -> Result<(), String> {
    let started = Instant::now();
    loop {
        match fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
                return Ok(());
            }
            Ok(_) => return Err(format!("{label} 必须是普通文件")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("等待 {label} 失败: {error}")),
        }
        if started.elapsed() >= SIGNAL_TIMEOUT {
            return Err(format!("等待 {label} 超时（180 秒）"));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// 读取 Session 目录内的两个权威文件，并按字节复制到独占阶段目录。
pub(super) fn capture_stage(
    fixture_root: &Path,
    data_root: &Path,
    session_id: &str,
    stage: usize,
) -> Result<(), String> {
    if session_id.is_empty() || session_id.contains(['/', '\\']) {
        return Err("ACP Session 标识不能包含路径分隔符".to_owned());
    }
    let source_dir = data_root.join("sessions").join(session_id);
    let stage_dir = fixture_root.join(format!("stage-{stage}"));
    fs::create_dir(&stage_dir).map_err(|error| format!("创建阶段目录失败: {error}"))?;
    for name in ["events.jsonl", "collaboration-v2.json"] {
        let source = source_dir.join(name);
        let before = fs::symlink_metadata(&source)
            .map_err(|error| format!("读取阶段源文件 {} 失败: {error}", source.display()))?;
        if before.file_type().is_symlink() || !before.is_file() {
            return Err(format!("阶段源文件 {} 必须是普通文件", source.display()));
        }
        let mut input = crate::storage::open_readonly_regular_file(&source)
            .map_err(|error| format!("安全打开阶段源文件 {} 失败: {error}", source.display()))?;
        let mut bytes = Vec::new();
        input
            .read_to_end(&mut bytes)
            .map_err(|error| format!("读取阶段源文件 {} 失败: {error}", source.display()))?;
        let after = fs::symlink_metadata(&source)
            .map_err(|error| format!("复核阶段源文件 {} 失败: {error}", source.display()))?;
        if after.file_type().is_symlink() || !after.is_file() || after.len() != bytes.len() as u64 {
            return Err(format!(
                "阶段源文件 {} 在读取期间发生变化",
                source.display()
            ));
        }
        write_bytes_create_new(&stage_dir.join(name), &bytes)?;
    }
    Ok(())
}

/// 所有正式 ACP 请求共用 60 秒超时，并保留命令返回的错误。
fn dispatch_with_timeout(request: Value) -> Result<Value, String> {
    let result = tauri::async_runtime::block_on(async {
        tokio::time::timeout(ACP_TIMEOUT, crate::acp_host::acp_dispatch(request))
            .await
            .map_err(|_| "ACP 请求超过 60 秒超时".to_owned())?
    })?;
    result.ok_or_else(|| "ACP 请求意外返回空响应".to_owned())
}

/// 在校验前封存完整请求和响应，失败也不能丢失原始协议证据。
pub(super) fn recorded_dispatch(root: &Path, label: &str, request: Value) -> Result<Value, String> {
    let response = dispatch_with_timeout(request.clone());
    write_json_create_new(
        &root.join(format!("{label}.json")),
        &json!({"request": request, "response": response.as_ref().ok(), "dispatchError": response.as_ref().err()}),
    )?;
    let response = response?;
    if response.get("jsonrpc") != Some(&json!("2.0")) || response.get("id") != request.get("id") {
        return Err(format!("{label} JSON-RPC 外层或响应身份不匹配"));
    }
    Ok(response)
}

/// 校验 JSON-RPC 成功响应；任何完整 error 都进入后台 Result，而不是被忽略。
pub(super) fn require_result<'a>(response: &'a Value, method: &str) -> Result<&'a Value, String> {
    if let Some(error) = response.get("error") {
        return Err(format!("{method} 返回 ACP error: {error}"));
    }
    response
        .get("result")
        .ok_or_else(|| format!("{method} 响应缺少 result"))
}

/// 从 session/new 的完整响应提取唯一 Session 标识。
pub(super) fn session_id_from_response(response: &Value) -> Result<String, String> {
    require_result(response, "session/new")?
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "session/new 响应缺少有效 sessionId".to_owned())
}

/// 核对 Prompt 的标准终态和 Host 快照，不把响应返回当成 Turn 已完成的替代品。
pub(super) fn assert_completed_prompt(
    response: &Value,
    session_id: &str,
    turn_id: &str,
) -> Result<(), String> {
    let result = require_result(response, "session/prompt")?;
    if result.get("stopReason").and_then(Value::as_str) != Some("end_turn") {
        return Err(format!("{turn_id} stopReason 不是 end_turn: {result}"));
    }
    let snapshot = result
        .get("_meta")
        .and_then(|meta| meta.get("keencode/snapshot"))
        .ok_or_else(|| format!("{turn_id} 缺少 keencode/snapshot"))?;
    if snapshot.get("sessionId").and_then(Value::as_str) != Some(session_id)
        || !snapshot.get("activeTurnId").is_some_and(Value::is_null)
        || !matches!(
            snapshot.get("state").and_then(Value::as_str),
            Some("idle" | "ready")
        )
    {
        return Err(format!(
            "{turn_id} 终态快照不符合 idle/ready 且无 activeTurnId: {snapshot}"
        ));
    }
    Ok(())
}

/// 执行握手、新建 Session 和三轮正式 root Prompt。
fn run_mailbox_protocol(
    fixture_root: &Path,
    data_root: &Path,
    workspace_root: &Path,
    operation_id: &str,
) -> Result<(String, usize), String> {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": "native-mailbox-initialize",
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientInfo": {"name": "KeenCode", "version": "0.0.1"},
            "clientCapabilities": {"elicitation": {"form": {}}}
        }
    });
    let initialize_response = recorded_dispatch(fixture_root, "host-initialize", initialize)?;
    require_result(&initialize_response, "initialize")?;

    let new_session = json!({
        "jsonrpc": "2.0",
        "id": "native-mailbox-session-new",
        "method": "session/new",
        "params": {
            "cwd": workspace_root,
            "mcpServers": [],
            "_meta": {"keencode/operationId": operation_id}
        }
    });
    let session_response = recorded_dispatch(fixture_root, "host-new", new_session)?;
    let session_id = session_id_from_response(&session_response)?;
    write_json_create_new(
        &fixture_root.join("host-ready.json"),
        &json!({
            "sessionId": session_id,
            "pid": std::process::id(),
            "workspace": workspace_root,
            "operationId": operation_id
        }),
    )?;

    wait_for_signal(&fixture_root.join("start"), "start")?;

    for (index, (turn_id, text)) in TURN_TEXTS.iter().enumerate() {
        let ordinal = index + 1;
        let mode = json!({
            "jsonrpc": "2.0",
            "id": format!("native-mailbox-mode-{ordinal}"),
            "method": "session/set_mode",
            "params": {
                "sessionId": session_id,
                "modeId": "default"
            }
        });
        let mode_response = recorded_dispatch(fixture_root, &format!("host-mode-{ordinal}"), mode)?;
        require_result(&mode_response, "session/set_mode")?;
        let prompt = json!({
            "jsonrpc": "2.0",
            "id": turn_id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": text}],
                "_meta": {"keencode/turnId": turn_id, "keencode/ultraMode": false}
            }
        });
        let prompt_response =
            recorded_dispatch(fixture_root, &format!("host-turn-{ordinal}"), prompt)?;
        assert_completed_prompt(&prompt_response, &session_id, turn_id)?;
        capture_stage(fixture_root, data_root, &session_id, ordinal)?;
        let signal = match ordinal {
            1 => "second",
            2 => "third",
            _ => "finish",
        };
        wait_for_signal(&fixture_root.join(signal), signal)?;
    }
    Ok((session_id, TURN_TEXTS.len()))
}

/// 三回合原生夹具的协议入口；共享宿主不接触 Runtime 内部状态。
pub(super) type NativeProtocol = fn(&Path, &Path, &Path, &str) -> Result<(String, usize), String>;

/// 后台线程捕获所有 Result，完成正式 recorder.flush 后才放行并结束本测试宿主。
fn run_worker(
    handle: AppHandle,
    fixture_root: PathBuf,
    data_root: PathBuf,
    workspace_root: PathBuf,
    result_slot: Arc<Mutex<Option<Value>>>,
    protocol: NativeProtocol,
) {
    let operation_id = format!("native-mailbox-session-{}", std::process::id());
    // 测试驱动即使 panic 也必须记录失败并收尾当前窗口，不能遗留永久 run_return。
    let protocol_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        protocol(&fixture_root, &data_root, &workspace_root, &operation_id)
    }))
    .unwrap_or_else(|_| Err("原生 mailbox 协议驱动发生 panic，原始日志保留".to_owned()));
    let recorder_result = handle
        .state::<Arc<crate::analytics::AnalyticsRecorder>>()
        .flush();
    let ok = protocol_result.is_ok() && recorder_result.is_ok();
    let error = protocol_result
        .as_ref()
        .err()
        .cloned()
        .or_else(|| recorder_result.as_ref().err().cloned());
    let (session_id, turns) = protocol_result
        .ok()
        .map_or((None, 0), |(session_id, turns)| (Some(session_id), turns));
    let result = json!({
        "ok": ok,
        "sessionId": session_id,
        "turnsCompleted": turns,
        "recorderFlush": recorder_result.as_ref().map(|_| "ok").unwrap_or("error"),
        "error": error,
        "teardown": "仅结束本 native mailbox 测试宿主"
    });
    if let Err(write_error) = write_json_create_new(&fixture_root.join("host-result.json"), &result)
    {
        *result_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(json!({
            "ok": false,
            "error": write_error,
            "teardown": "测试证据写入失败，仍仅请求结束本测试宿主"
        }));
    } else {
        *result_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
    }
    // approve/exit 是测试收尾；正式退出事件仍由主线程统一交给 handle_run_event。
    handle.state::<crate::app_exit::ExitState>().approve();
    handle.exit(if ok { 0 } else { 1 });
}

/// 复用正式桌面装配和事件入口的真实 mailbox 原生测试。
#[test]
#[ignore = "需要显式 Windows 原生夹具、外部 Node 服务和真实桌面 Runtime"]
fn native_mailbox_root_turns_use_formal_acp_dispatch() {
    run_native_protocol_fixture("KEENCODE_NATIVE_MAILBOX_FIXTURE_DIR", run_mailbox_protocol);
}

/// 在相同隔离约束、正式 Builder 和退出路径下运行三回合测试协议。
pub(super) fn run_native_protocol_fixture(variable: &str, protocol: NativeProtocol) {
    let fixture_root =
        fixture_root_from_environment(variable).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(std::env::var("KEENCODE_BENCHMARK").as_deref(), Ok("1"));
    let data_root = PathBuf::from(
        std::env::var_os("KEENCODE_BENCHMARK_DATA_DIR").expect("必须指定隔离数据目录"),
    );
    let webview_root = PathBuf::from(
        std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").expect("必须指定隔离 WebView 目录"),
    );
    assert_exact_fixture_child(&data_root, &fixture_root, "data").unwrap();
    assert_exact_fixture_child(&webview_root, &fixture_root, "webview").unwrap();
    let workspace_root = fixture_root.join("workspace");
    require_directory(&data_root, "benchmark data").unwrap();
    require_directory(&workspace_root, "workspace").unwrap();
    assert_absent(&webview_root, "WebView 用户数据目录").unwrap();
    for path in [
        "start",
        "second",
        "third",
        "finish",
        "host-ready.json",
        "host-turn-1.json",
        "host-turn-2.json",
        "host-turn-3.json",
        "host-result.json",
    ] {
        assert_absent(&fixture_root.join(path), path).unwrap();
    }
    for stage in 1..=3 {
        assert_absent(
            &fixture_root.join(format!("stage-{stage}")),
            &format!("stage-{stage}"),
        )
        .unwrap();
    }

    crate::configure_before_start();
    let ready_seen = Arc::new(AtomicBool::new(false));
    let worker_slot = Arc::new(Mutex::new(None::<thread::JoinHandle<()>>));
    let result_slot = Arc::new(Mutex::new(None::<Value>));
    let app = crate::desktop_builder(Instant::now())
        .any_thread()
        .build(tauri::generate_context!())
        .expect("应能创建正式 mailbox 原生测试宿主");
    let ready_for_run = Arc::clone(&ready_seen);
    let worker_for_run = Arc::clone(&worker_slot);
    let result_for_run = Arc::clone(&result_slot);
    let fixture_for_run = fixture_root.clone();
    let data_for_run = data_root.clone();
    let workspace_for_run = workspace_root.clone();
    let exit_code = app.run_return(move |app, event| {
        if matches!(&event, tauri::RunEvent::Ready) && !ready_for_run.swap(true, Ordering::AcqRel) {
            let handle = app.clone();
            let fixture = fixture_for_run.clone();
            let data = data_for_run.clone();
            let workspace = workspace_for_run.clone();
            let result = Arc::clone(&result_for_run);
            let worker = thread::Builder::new()
                .name("native-mailbox-protocol".to_owned())
                .spawn(move || run_worker(handle, fixture, data, workspace, result, protocol));
            match worker {
                Ok(worker) => {
                    *worker_for_run
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(worker)
                }
                Err(error) => {
                    let result =
                        json!({"ok": false, "error": format!("创建 mailbox worker 失败: {error}")});
                    let _ =
                        write_json_create_new(&fixture_for_run.join("host-result.json"), &result);
                    *result_for_run
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
                    app.state::<crate::app_exit::ExitState>().approve();
                    app.exit(1);
                }
            }
        }
        crate::handle_run_event(app, event);
    });
    if let Some(worker) = worker_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        assert!(worker.join().is_ok(), "mailbox worker 不应 panic");
    }
    assert!(ready_seen.load(Ordering::Acquire), "正式桌面必须收到 Ready");
    let result = result_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("host-result.json 对应的后台 Result 必须存在");
    assert_eq!(exit_code, 0, "mailbox 宿主失败: {result}");
    assert_eq!(result["ok"], true, "mailbox 结果未通过: {result}");
    assert_eq!(result["turnsCompleted"], 3);
}
