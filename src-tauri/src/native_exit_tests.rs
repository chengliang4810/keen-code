//! 仅在显式运行的 Windows 测试二进制内验证真实桌面退出失败链路。
//!
//! 通过断开的记录器通道注入真实 flush 错误；不修改正式配置、磁盘权限或生产 IPC。
//! 使用 `cargo test --manifest-path src-tauri/Cargo.toml --lib --features native-desktop-tests
//! native_exit_tests::native_exit_failure_keeps_window_open_without_retry -- --ignored --exact
//! --test-threads=1 --nocapture`，并提前设置下面列出的四个隔离环境变量。
//! `finish` 仅是控制端收尾信号，UI 结论必须同时附带实际点击记录和前后截图。

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tauri::{Listener, Manager};

/// 独占写入测试证据，拒绝覆盖任何先前运行的结果。
fn write_evidence(path: &Path, value: &Value) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .expect("原生验收证据必须独占创建");
    serde_json::to_writer_pretty(&mut file, value).expect("验收证据应能序列化");
    writeln!(file).expect("验收证据应能写入末尾换行");
    file.sync_all().expect("验收证据必须完成同步");
}

/// 验证原生窗口关闭后错误事件真实到达前端；需要人工或受控桌面操作点击关闭及知道了。
///
/// 运行条件：隔离的 KEENCODE_NATIVE_EXIT_FIXTURE_DIR、BENCHMARK 数据目录和 WebView 目录；
/// 测试捕获一次失败后等待根验收端独占创建 finish 文件，最多三分钟，随后只收尾本测试进程。
#[test]
#[ignore = "需要显式的隔离目录及 Windows 原生桌面交互，不属于常规无界面测试"]
fn native_exit_failure_keeps_window_open_without_retry() {
    let fixture_root = PathBuf::from(
        std::env::var_os("KEENCODE_NATIVE_EXIT_FIXTURE_DIR").expect("必须明确指定原生隔离验收根"),
    )
    .canonicalize()
    .expect("原生隔离根必须已存在");
    assert_eq!(std::env::var("KEENCODE_BENCHMARK").as_deref(), Ok("1"));
    let data_root = PathBuf::from(
        std::env::var_os("KEENCODE_BENCHMARK_DATA_DIR").expect("必须指定隔离数据目录"),
    );
    let webview_root = PathBuf::from(
        std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").expect("必须指定隔离 WebView 目录"),
    );
    // 比较已存在父目录的规范路径，兼容 Windows canonicalize 的扩展长度前缀。
    for (directory, expected_name) in [(&data_root, "data"), (&webview_root, "webview")] {
        assert!(directory.is_absolute(), "隔离目录必须为绝对路径");
        assert_eq!(
            directory.file_name(),
            Some(std::ffi::OsStr::new(expected_name))
        );
        assert_eq!(
            directory
                .parent()
                .expect("隔离目录必须拥有父目录")
                .canonicalize()
                .expect("隔离目录的父目录必须存在"),
            fixture_root,
        );
    }
    assert!(!data_root.exists(), "不能复用已有数据目录");
    assert!(!webview_root.exists(), "不能复用已有 WebView 目录");
    assert!(!fixture_root.join("finish").exists());
    assert!(!fixture_root.join("ready.json").exists());
    assert!(!fixture_root.join("result.json").exists());

    crate::configure_before_start();
    let observed_errors = Arc::new(Mutex::new(Vec::<Value>::new()));
    let controller_result = Arc::new(Mutex::new(None::<Value>));
    let errors_for_app = Arc::clone(&observed_errors);
    let result_for_app = Arc::clone(&controller_result);
    let fixture_for_app = fixture_root.clone();
    let app = crate::desktop_builder(Instant::now())
        .any_thread()
        .build(tauri::generate_context!())
        .expect("应能创建正式桌面的原生测试宿主");

    // Ready 在正式 setup 完成后执行；所有正式命令、事件与前端资源保持原样。
    let exit_code = app.run_return(move |app, event| {
        if matches!(&event, tauri::RunEvent::Ready) {
            let recorder = app.state::<Arc<crate::analytics::AnalyticsRecorder>>();
            recorder.flush().expect("正式记录器注入前必须能正常同步");
            recorder
                .disconnect_writer_for_test()
                .expect("测试接收端必须已断开");
            let captured_errors = Arc::clone(&errors_for_app);
            app.listen("app://exit-failed", move |event| {
                let payload: Value =
                    serde_json::from_str(event.payload()).expect("退出失败事件应为合法 JSON");
                captured_errors
                    .lock()
                    .expect("原生错误记录锁不应被污染")
                    .push(payload);
            });
            let window = app
                .get_webview_window("main")
                .expect("正式桌面主窗口应已创建");
            write_evidence(
                &fixture_for_app.join("ready.json"),
                &json!({
                    "pid": std::process::id(),
                    "windowScaleFactor": window.scale_factor().expect("应能读取原生窗口缩放"),
                    "expectedFailure": "模型请求记录 writer 已退出",
                    "normalRecorderFlushBeforeInjectionSucceeded": true,
                    "testOnlyInjection": "断开记录器队列，保留生产退出、事件和界面逻辑",
                }),
            );

            let handle = app.clone();
            let fixture = fixture_for_app.clone();
            let errors = Arc::clone(&errors_for_app);
            let result = Arc::clone(&result_for_app);
            std::thread::spawn(move || {
                // 只轮询隔离夹具的完成信号；正式应用没有此线程或故障开关。
                let started = Instant::now();
                let finished = loop {
                    if fixture.join("finish").is_file() {
                        break true;
                    }
                    if started.elapsed() >= Duration::from_secs(180) {
                        break false;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                };
                let events = errors.lock().expect("错误记录锁应可用").clone();
                let approved = handle.state::<crate::app_exit::ExitState>().is_approved();
                let window_present = handle.get_webview_window("main").is_some();
                let records = fs::metadata(fixture.join("data/model-request-records.jsonl"))
                    .map(|metadata| metadata.len())
                    .ok();
                // 同一个正式记录器仍须失败；关闭提示不能恢复记录器或自动重试退出。
                let repeated_flush = handle
                    .state::<Arc<crate::analytics::AnalyticsRecorder>>()
                    .flush();
                let observation = json!({
                    "finishedByController": finished,
                    "errors": events,
                    "exitApprovedBeforeTeardown": approved,
                    "windowPresentBeforeTeardown": window_present,
                    "modelRecordBytes": records,
                    "recorderFlushAfterDismissError": repeated_flush.err(),
                    "teardown": "测试专用放行并结束当前宿主，不计为生产正常退出成功",
                });
                write_evidence(&fixture.join("result.json"), &observation);
                *result.lock().expect("夹具结果锁应可用") = Some(observation);
                // 故障证据封存后释放测试宿主；不恢复生产 Runtime，也不触碰其他进程。
                handle.state::<crate::app_exit::ExitState>().approve();
                handle.exit(0);
            });
        }
        crate::handle_run_event(app, event);
    });

    assert_eq!(exit_code, 0);
    let result = controller_result
        .lock()
        .expect("夹具结果锁应可用")
        .clone()
        .expect("夹具必须记录实际退出前状态");
    assert_eq!(result["finishedByController"], true);
    assert_eq!(result["exitApprovedBeforeTeardown"], false);
    assert_eq!(result["windowPresentBeforeTeardown"], true);
    assert_eq!(result["modelRecordBytes"], 0);
    assert_eq!(
        result["recorderFlushAfterDismissError"],
        "模型请求记录 writer 已退出"
    );
    let errors = result["errors"].as_array().expect("错误事件应为数组");
    assert_eq!(errors.len(), 1, "知道了不应再次请求退出或重复发送失败");
    assert_eq!(errors[0]["message"], "模型请求记录 writer 已退出");
}
