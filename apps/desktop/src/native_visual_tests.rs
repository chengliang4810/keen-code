//! 仅在显式运行的 Windows 测试二进制内记录真实桌面窗口的前端与原生尺寸。
//!
//! 测试复用正式桌面装配和正式退出事件处理，只在本测试进程中注册一次
//! native-visual-metrics-* 事件监听器，并通过 Tauri 事件插件上传只读窗口指标。
//! 运行时需要显式设置 KEENCODE_NATIVE_VISUAL_FIXTURE_DIR、KEENCODE_BENCHMARK=1、
//! KEENCODE_BENCHMARK_DATA_DIR 和 WEBVIEW2_USER_DATA_FOLDER；夹具脚本只作 document-start
//! 注入且夹具脚本不主动联网，不修改生产装配，并允许夹具根下预置真实存储层可读取的合成 Journal 数据。

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri::{Listener, Manager, webview::PageLoadEvent};

/// 可选的启动前脚本及其不可变摘要，只允许来自夹具根目录的普通文件。
#[derive(Clone)]
struct BootstrapFixture {
    /// 是否发现了夹具根目录下的 bootstrap.js。
    present: bool,
    /// bootstrap.js 的原始字节；缺失时为空。
    bytes: Vec<u8>,
    /// bootstrap.js 的 SHA-256；缺失时为空。
    sha256: Option<String>,
}

/// 用 create_new 独占写入 JSON，防止测试证据被覆盖或静默拼接。
fn write_json_create_new(path: &Path, value: &Value) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("创建证据文件失败 {}: {error}", path.display()))?;
    serde_json::to_writer_pretty(&mut file, value)
        .map_err(|error| format!("序列化证据失败 {}: {error}", path.display()))?;
    writeln!(file).map_err(|error| format!("写入证据换行失败 {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("同步证据文件失败 {}: {error}", path.display()))
}

/// 计算夹具脚本的 SHA-256，供 ready.json 记录实际注入字节。
fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

/// 读取可选 bootstrap.js，并拒绝符号链接、目录和其它非普通文件。
fn load_optional_bootstrap(path: &Path) -> BootstrapFixture {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return BootstrapFixture {
                present: false,
                bytes: Vec::new(),
                sha256: None,
            };
        }
        Err(error) => panic!("检查 bootstrap.js 失败 {}: {error}", path.display()),
    };
    assert!(
        !metadata.file_type().is_symlink() && metadata.is_file(),
        "bootstrap.js 必须是夹具根目录下的普通文件"
    );
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("读取 bootstrap.js 失败: {error}"));
    let final_metadata = fs::symlink_metadata(path).expect("读取 bootstrap.js 后应能复核文件");
    assert!(
        !final_metadata.file_type().is_symlink() && final_metadata.is_file(),
        "bootstrap.js 读取期间不能变为符号链接或其它文件"
    );
    assert_eq!(
        final_metadata.len(),
        bytes.len() as u64,
        "bootstrap.js 读取长度发生变化"
    );
    BootstrapFixture {
        present: true,
        sha256: Some(sha256_hex(&bytes)),
        bytes,
    }
}

/// 读取并规范化原生视觉夹具根；根目录本身不能是符号链接。
fn fixture_root_from_environment() -> PathBuf {
    let raw = PathBuf::from(
        std::env::var_os("KEENCODE_NATIVE_VISUAL_FIXTURE_DIR")
            .expect("必须明确指定原生视觉隔离夹具根"),
    );
    assert!(raw.is_absolute(), "原生视觉夹具根必须是绝对路径");
    let metadata = fs::symlink_metadata(&raw).expect("原生视觉夹具根必须已存在");
    assert!(
        !metadata.file_type().is_symlink() && metadata.is_dir(),
        "原生视觉夹具根必须是非符号链接目录"
    );
    raw.canonicalize().expect("原生视觉夹具根必须能规范化")
}

/// 校验环境目录是 fixtureRoot 的直接子目录，拒绝相对路径和路径穿越。
fn assert_exact_fixture_child(path: &Path, fixture_root: &Path, expected_name: &str) {
    assert!(path.is_absolute(), "隔离目录必须为绝对路径");
    assert!(
        !path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir)),
        "隔离目录不能包含 . 或 .."
    );
    assert_eq!(
        path.file_name(),
        Some(std::ffi::OsStr::new(expected_name)),
        "隔离目录必须命名为 {expected_name}"
    );
    assert_eq!(
        path.parent()
            .expect("隔离目录必须拥有父目录")
            .canonicalize()
            .expect("隔离目录的父目录必须存在且可规范化"),
        fixture_root,
        "隔离目录必须是夹具根的直接子目录"
    );
}

/// 目录若已存在只能是普通目录；不存在时交给正式运行时按需创建。
fn assert_directory_if_present(path: &Path, label: &str) {
    match fs::symlink_metadata(path) {
        Ok(metadata) => assert!(
            !metadata.file_type().is_symlink() && metadata.is_dir(),
            "{label} 已存在但不是普通目录"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("检查 {label} 失败: {error}"),
    }
}

/// 拒绝任意类型的既有路径，包括悬空符号链接，避免复用上一次夹具结果。
fn assert_absent(path: &Path, label: &str) {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => panic!("{label} 已存在，拒绝复用: {}", path.display()),
        Err(error) => panic!("检查 {label} 失败: {error}"),
    }
}

/// 校验前端上传的指标只包含本探针读取的数字和字体状态。
fn validate_metric(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "视觉指标必须是 JSON 对象".to_owned())?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| "视觉指标缺少 kind 状态".to_owned())?;
    if kind != "initial" && kind != "resize" && kind != "input" {
        return Err(format!("未知视觉指标 kind: {kind}"));
    }
    // 被动输入诊断必须显式携带事件记录，不能冒充窗口 resize 或模型执行证据。
    if kind == "input" && !object.get("inputTrace").is_some_and(Value::is_object) {
        return Err("输入诊断缺少 inputTrace 对象".to_owned());
    }
    for key in [
        "devicePixelRatio",
        "innerWidth",
        "innerHeight",
        "outerWidth",
        "outerHeight",
        "performanceTimeOrigin",
        "performanceNow",
    ] {
        let number = object
            .get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("视觉指标 {key} 必须是数字"))?;
        if !number.is_finite() {
            return Err(format!("视觉指标 {key} 不是有限数字"));
        }
    }
    let fonts_status = object
        .get("documentFontsStatus")
        .and_then(Value::as_str)
        .ok_or_else(|| "视觉指标缺少 documentFontsStatus".to_owned())?;
    if fonts_status.is_empty() {
        return Err("documentFontsStatus 不能为空".to_owned());
    }
    Ok(())
}

/// 将前端单条指标校验后以唯一文件名写入 metrics 目录。
fn write_metric(metrics_dir: &Path, ordinal: u64, value: &Value) -> Result<(), String> {
    validate_metric(value)?;
    let kind = value["kind"]
        .as_str()
        .ok_or_else(|| "视觉指标 kind 不是字符串".to_owned())?;
    let path = metrics_dir.join(format!("metric-{ordinal:06}-{kind}.json"));
    write_json_create_new(&path, value)
}

/// 生成在页面完成加载后执行的最小只读探针。
///
/// @tauri-apps/api/event.js 的 emit 实现调用的就是
/// window.__TAURI_INTERNALS__.invoke("plugin:event|emit", { event, payload })；
/// 此处只复用该已确认的事件协议，不动态导入脚本、不调用 bootstrap 提供的函数。
fn metric_probe_script(event_name: &str) -> String {
    let event_literal = serde_json::to_string(event_name).expect("事件名应能序列化为 JS 字符串");
    format!(
        r#"(function () {{
  const eventName = {event_literal};
  const emitMetric = (kind) => {{
    const payload = {{
      kind,
      devicePixelRatio: window.devicePixelRatio,
      innerWidth: window.innerWidth,
      innerHeight: window.innerHeight,
      outerWidth: window.outerWidth,
      outerHeight: window.outerHeight,
      performanceTimeOrigin: performance.timeOrigin,
      performanceNow: performance.now(),
      documentFontsStatus: document.fonts.status
    }};
    window.__TAURI_INTERNALS__.invoke("plugin:event|emit", {{
      event: eventName,
      payload
    }}).catch(() => {{}});
  }};
  window.addEventListener("resize", () => emitMetric("resize"), {{ passive: true }});
  document.fonts.ready.then(() =>
    requestAnimationFrame(() => requestAnimationFrame(() => emitMetric("initial")))
  );
}})();"#,
    )
}

/// 列出 metrics 下的普通文件，并拒绝测试期间出现的符号链接条目。
fn metric_files(metrics_dir: &Path) -> Vec<String> {
    let mut files = fs::read_dir(metrics_dir)
        .expect("metrics 目录应可读取")
        .map(|entry| {
            let entry = entry.expect("metrics 目录条目应可读取");
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("metrics 条目应可复核");
            assert!(
                !metadata.file_type().is_symlink() && metadata.is_file(),
                "metrics 目录只能包含普通 JSON 文件"
            );
            path.file_name()
                .expect("metrics 文件必须有文件名")
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// 记录一次真实 native 视觉尺寸快照；需要外部截图/关闭操作，不属于常规测试。
#[test]
#[ignore = "需要显式 Windows 原生桌面夹具和用户可见窗口，不属于常规无界面测试"]
fn native_visual_fixture_records_actual_window_metrics() {
    let fixture_root = fixture_root_from_environment();
    assert_eq!(std::env::var("KEENCODE_BENCHMARK").as_deref(), Ok("1"));

    // storage.rs 使用的真实隔离变量名是 KEENCODE_BENCHMARK_DATA_DIR；其值必须是夹具根直接子目录。
    let data_root = PathBuf::from(
        std::env::var_os("KEENCODE_BENCHMARK_DATA_DIR").expect("必须指定隔离基准数据目录"),
    );
    let webview_root = PathBuf::from(
        std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").expect("必须指定隔离 WebView 目录"),
    );
    assert_exact_fixture_child(&data_root, &fixture_root, "data");
    assert_exact_fixture_child(&webview_root, &fixture_root, "webview");
    assert_directory_if_present(&data_root, "benchmark data");
    assert_absent(&data_root.join("logs"), "benchmark data/logs");
    assert_absent(&webview_root, "WebView 用户数据目录");

    let ready_path = fixture_root.join("ready.json");
    let result_path = fixture_root.join("result.json");
    let metrics_dir = fixture_root.join("metrics");
    assert_absent(&ready_path, "ready.json");
    assert_absent(&result_path, "result.json");
    assert_absent(&metrics_dir, "metrics 目录");
    fs::create_dir(&metrics_dir).expect("应能创建独立 metrics 目录");

    // 只读取 fixtureRoot/bootstrap.js；存在时以测试专用 Tauri plugin 的 document-start 脚本注入。
    let bootstrap_path = fixture_root.join("bootstrap.js");
    let bootstrap = load_optional_bootstrap(&bootstrap_path);
    let bootstrap_script = if bootstrap.present {
        Some(
            String::from_utf8(bootstrap.bytes.clone())
                .expect("bootstrap.js 必须是 UTF-8 JavaScript 文件"),
        )
    } else {
        None
    };

    crate::configure_before_start();

    let metric_event_name = format!("native-visual-metrics-{}", std::process::id());
    let listener_registered = Arc::new(AtomicBool::new(false));
    let metrics_written = Arc::new(AtomicU64::new(0));
    let metric_sequence = Arc::new(AtomicU64::new(0));
    let listener_errors = Arc::new(Mutex::new(Vec::<String>::new()));

    let mut builder = crate::desktop_builder(Instant::now()).any_thread();
    if let Some(script) = bootstrap_script {
        builder = builder.plugin(
            tauri::plugin::Builder::<tauri::Wry, ()>::new("native-visual-fixture")
                .js_init_script(script)
                .build(),
        );
    }

    // on_page_load 的 Finished 回调先注册唯一 Rust listener，再 eval 探针，避免丢失首条指标。
    let listener_registered_for_load = Arc::clone(&listener_registered);
    let metric_sequence_for_load = Arc::clone(&metric_sequence);
    let metrics_written_for_load = Arc::clone(&metrics_written);
    let listener_errors_for_load = Arc::clone(&listener_errors);
    let metrics_dir_for_load = metrics_dir.clone();
    let metric_event_name_for_load = metric_event_name.clone();
    builder = builder.on_page_load(move |webview, payload| {
        if payload.event() != PageLoadEvent::Finished
            || listener_registered_for_load.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let app_handle = webview.app_handle().clone();
        let metric_sequence_for_event = Arc::clone(&metric_sequence_for_load);
        let metrics_written_for_event = Arc::clone(&metrics_written_for_load);
        let listener_errors_for_event = Arc::clone(&listener_errors_for_load);
        let metrics_dir_for_event = metrics_dir_for_load.clone();
        let app_handle_for_event = app_handle.clone();
        // 视觉与被动输入诊断复用同一落盘回调；二者都只存在于本测试宿主。
        let metric_listener = move |event: tauri::Event| {
            let mut payload = match serde_json::from_str::<Value>(event.payload()) {
                Ok(payload) => payload,
                Err(error) => {
                    listener_errors_for_event
                        .lock()
                        .expect("视觉 listener 错误锁应可用")
                        .push(format!("事件 JSON 无效: {error}"));
                    return;
                }
            };
            let window = match app_handle_for_event.get_webview_window("main") {
                Some(window) => window,
                None => {
                    listener_errors_for_event
                        .lock()
                        .expect("视觉 listener 错误锁应可用")
                        .push("收到视觉指标时正式主窗口不存在".to_owned());
                    return;
                }
            };
            let scale_factor = match window.scale_factor() {
                Ok(scale_factor) => scale_factor,
                Err(error) => {
                    listener_errors_for_event
                        .lock()
                        .expect("视觉 listener 错误锁应可用")
                        .push(format!("读取视觉指标原生缩放失败: {error}"));
                    return;
                }
            };
            let inner_size = match window.inner_size() {
                Ok(inner_size) => inner_size,
                Err(error) => {
                    listener_errors_for_event
                        .lock()
                        .expect("视觉 listener 错误锁应可用")
                        .push(format!("读取视觉指标原生客户区尺寸失败: {error}"));
                    return;
                }
            };
            let outer_size = match window.outer_size() {
                Ok(outer_size) => outer_size,
                Err(error) => {
                    listener_errors_for_event
                        .lock()
                        .expect("视觉 listener 错误锁应可用")
                        .push(format!("读取视觉指标原生窗口尺寸失败: {error}"));
                    return;
                }
            };
            let Some(object) = payload.as_object_mut() else {
                listener_errors_for_event
                    .lock()
                    .expect("视觉 listener 错误锁应可用")
                    .push("视觉指标必须是 JSON 对象".to_owned());
                return;
            };
            object.insert(
                "nativeWindow".to_owned(),
                json!({
                    "scaleFactor": scale_factor,
                    "innerSize": { "width": inner_size.width, "height": inner_size.height },
                    "outerSize": { "width": outer_size.width, "height": outer_size.height }
                }),
            );
            let ordinal = metric_sequence_for_event.fetch_add(1, Ordering::AcqRel) + 1;
            match write_metric(&metrics_dir_for_event, ordinal, &payload) {
                Ok(()) => {
                    metrics_written_for_event.fetch_add(1, Ordering::Release);
                }
                Err(error) => listener_errors_for_event
                    .lock()
                    .expect("视觉 listener 错误锁应可用")
                    .push(error),
            }
        };
        let _listener_id =
            app_handle.listen(metric_event_name_for_load.clone(), metric_listener.clone());
        // 固定测试事件名避免夹具替换不可写的 Tauri IPC 桥；应用业务仍使用原始桥。
        let _input_listener_id = app_handle.listen("native-visual-input", metric_listener);
        webview
            .eval(metric_probe_script(&metric_event_name_for_load))
            .expect("原生视觉只读探针应能注入页面");
    });

    let app = builder
        // 必须复用正式的原样 Tauri 上下文，不替换为测试配置或模拟窗口。
        .build(tauri::generate_context!())
        .expect("应能创建正式桌面的原生视觉测试宿主");

    let ready_seen = Arc::new(AtomicBool::new(false));
    let ready_seen_for_run = Arc::clone(&ready_seen);
    let ready_path_for_run = ready_path.clone();
    let fixture_root_for_run = fixture_root.clone();
    let data_root_for_run = data_root.clone();
    let webview_root_for_run = webview_root.clone();
    let metrics_dir_for_run = metrics_dir.clone();
    let bootstrap_for_run = bootstrap.clone();
    let bootstrap_path_for_run = bootstrap_path.clone();
    let metric_event_name_for_run = metric_event_name.clone();

    // 只针对当前测试实例的十分钟 watchdog；超时走非正常退出并在结果中明确标记。
    let watchdog_stop = Arc::new(AtomicBool::new(false));
    let watchdog_timed_out = Arc::new(AtomicBool::new(false));
    let watchdog_stop_for_thread = Arc::clone(&watchdog_stop);
    let watchdog_timed_out_for_thread = Arc::clone(&watchdog_timed_out);
    let watchdog_handle = app.handle().clone();
    let watchdog = thread::spawn(move || {
        let started = Instant::now();
        loop {
            if watchdog_stop_for_thread.load(Ordering::Acquire) {
                return;
            }
            if started.elapsed() >= Duration::from_secs(600) {
                watchdog_timed_out_for_thread.store(true, Ordering::Release);
                // 仅为结束本测试宿主临时放行；result.json 会把它标记为超时失败。
                watchdog_handle
                    .state::<crate::app_exit::ExitState>()
                    .approve();
                watchdog_handle.exit(1);
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
    });

    // Ready 只写 PID 和真实原生窗口元数据；bootstrap 摘要用于证明测试注入内容的边界。
    let exit_code = app.run_return(move |app, event| {
        if matches!(&event, tauri::RunEvent::Ready)
            && !ready_seen_for_run.swap(true, Ordering::AcqRel)
        {
            let window = app
                .get_webview_window("main")
                .expect("正式桌面主窗口应已创建");
            let scale_factor = window.scale_factor().expect("应能读取原生窗口缩放");
            let inner_size = window.inner_size().expect("应能读取原生客户区尺寸");
            let outer_size = window.outer_size().expect("应能读取原生窗口尺寸");
            write_json_create_new(
                &ready_path_for_run,
                &json!({
                    "pid": std::process::id(),
                    "windowLabel": "main",
                    "window": {
                        "scaleFactor": scale_factor,
                        "innerSize": { "width": inner_size.width, "height": inner_size.height },
                        "outerSize": { "width": outer_size.width, "height": outer_size.height }
                    },
                    "bootstrap": {
                        "present": bootstrap_for_run.present,
                        "path": bootstrap_path_for_run,
                        "byteLength": bootstrap_for_run.bytes.len(),
                        "sha256": bootstrap_for_run.sha256,
                        "injected": bootstrap_for_run.present
                    },
                    "eventName": metric_event_name_for_run,
                    "metricsDirectory": metrics_dir_for_run,
                    "environment": {
                        "KEENCODE_BENCHMARK": "1",
                        "KEENCODE_BENCHMARK_DATA_DIR": data_root_for_run,
                        "WEBVIEW2_USER_DATA_FOLDER": webview_root_for_run,
                        "fixtureRoot": fixture_root_for_run
                    }
                }),
            )
            .expect("ready.json 必须独占写入");
        }
        // 正常窗口关闭始终经过正式生产退出事件入口；测试不实现第二套退出逻辑。
        crate::handle_run_event(app, event);
    });

    // run_return 已结束后立即停止 watchdog，正常退出绝不会被它改写为超时。
    watchdog_stop.store(true, Ordering::Release);
    watchdog.join().expect("视觉测试 watchdog 线程应正常结束");

    let timed_out = watchdog_timed_out.load(Ordering::Acquire);
    let files = metric_files(&metrics_dir);
    let listener_errors = listener_errors
        .lock()
        .expect("视觉 listener 错误锁应可用")
        .clone();
    let metrics_non_empty = !files.is_empty();
    let passed = ready_seen.load(Ordering::Acquire)
        && exit_code == 0
        && !timed_out
        && metrics_non_empty
        && listener_errors.is_empty();

    // 先写完整结果，再断言；超时或空 metrics 也必须留下可审阅的失败原因。
    write_json_create_new(
        &result_path,
        &json!({
            "passed": passed,
            "exitCode": exit_code,
            "timedOut": timed_out,
            "normalClose": !timed_out && exit_code == 0,
            "productionHandleRunEventUsed": true,
            "readySeen": ready_seen.load(Ordering::Acquire),
            "metricsDirectory": metrics_dir,
            "metricFileCount": files.len(),
            "metricsWrittenByListener": metrics_written.load(Ordering::Acquire),
            "metricFiles": files,
            "listenerErrors": listener_errors,
            "teardown": if timed_out {
                "测试专用 watchdog 强制收尾，不计为正常原生关闭"
            } else {
                "用户/外部桌面操作触发的关闭经过正式 handle_run_event"
            }
        }),
    )
    .expect("result.json 必须独占写入");

    assert!(
        ready_seen.load(Ordering::Acquire),
        "原生视觉测试未收到 Ready"
    );
    assert!(!timed_out, "原生视觉测试超过十分钟 watchdog，已标记为失败");
    assert_eq!(exit_code, 0, "原生视觉宿主非正常退出");
    assert!(metrics_non_empty, "原生视觉测试未收到任何前端尺寸指标");
    assert!(listener_errors.is_empty(), "原生视觉指标监听器出现错误");
}
