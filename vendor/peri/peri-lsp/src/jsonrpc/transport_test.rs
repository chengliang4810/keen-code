//! 测试 transport 分发与关闭语义

use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

use super::*;

#[tokio::test]
async fn test_server_request_unknown_id_receives_method_not_found() {
    // 服务器发起的请求（id 未注册 pending）：必须回 -32601 响应，
    // 而不是静默丢弃——否则服务器同步等待，后续 textDocument 请求排队至超时
    let (command, args, env) = peri_test_support::lsp_test_server("unknown-request");
    let transport = LspTransport::spawn(&command, &args, &env).expect("启动伪服务器失败");

    let (dispatcher, rx) = MessageDispatcher::new(transport);
    let state = dispatcher.dispatch_state();
    tokio::spawn(async move { run_dispatch_loop(state, rx).await });

    // 伪服务器收到 -32601 响应后自行退出（exit 0），否则 exit 1
    let child = Arc::clone(&dispatcher.child);
    let status = tokio::time::timeout(Duration::from_secs(5), async move {
        child.lock().await.as_mut().unwrap().wait().await
    })
    .await
    .expect("伪服务器未在 5s 内收到 -32601 响应并退出")
    .expect("wait 子进程失败");
    assert!(
        status.success(),
        "伪服务器应收到 -32601 响应（当前为静默丢弃）: {status:?}"
    );

    dispatcher.close().await;
}

#[tokio::test]
async fn test_cancel_request_removes_pending_entry() {
    // 超时/发送失败路径调用 cancel_request 后，pending 不得残留 oneshot sender
    // （此前仅在 transport EOF 时由 reject_all_pending 整体清理）
    let dispatcher = MessageDispatcher {
        dispatch_state: Arc::new(DispatchState {
            pending: Mutex::new(HashMap::new()),
            notification_handlers: Mutex::new(HashMap::new()),
            on_error: Mutex::new(None),
            stdin: tokio::sync::Mutex::new(None),
        }),
        read_task: Mutex::new(None),
        child: Arc::new(tokio::sync::Mutex::new(None)),
    };

    // receiver 保持存活，模拟"请求方仍持有 receiver 但已超时放弃"
    let _receiver = dispatcher.register_request(7);
    assert_eq!(dispatcher.dispatch_state().pending_len(), 1);

    dispatcher.cancel_request(7);
    assert_eq!(
        dispatcher.dispatch_state().pending_len(),
        0,
        "cancel_request 应移除 pending 条目"
    );

    // 取消不存在的 id（响应恰好已在途中被 dispatch 移除）应为无副作用 no-op
    dispatcher.cancel_request(7);
}

#[tokio::test]
async fn test_close_kills_child_process() {
    // Rust 测试 fixture 长驻 60 秒：close() 必须先 kill 子进程再 abort read task，
    // 否则 abort 路径跳过 child.kill()，子进程成为孤儿。
    let (command, args, env) = peri_test_support::lsp_test_server("sleep");
    let transport = LspTransport::spawn(&command, &args, &env).expect("启动失败");
    let (dispatcher, _rx) = MessageDispatcher::new(transport);

    dispatcher.close().await;

    let child = Arc::clone(&dispatcher.child);
    let status = tokio::time::timeout(Duration::from_secs(5), async move {
        child.lock().await.as_mut().unwrap().wait().await
    })
    .await
    .expect("close() 后子进程未在 5s 内退出（孤儿进程）")
    .expect("wait 子进程失败");
    // Unix 上 kill 以信号终止（code() 为 None）；Windows 上 TerminateProcess
    // 的退出码非 0——统一断言"非正常退出"以区分自然结束
    assert!(
        !status.success(),
        "子进程应被 close() 的 kill 终止，而非自然退出: {status:?}"
    );
}

/// 等待测试 fixture 写入进程树中的子进程 PID 文件。
#[cfg(any(unix, windows))]
async fn wait_for_file(path: &Path) {
    for _ in 0..200 {
        if path.is_file() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("测试 fixture 未在限定时间内写入文件: {}", path.display());
}

/// 构造会启动孙进程的 LSP transport，供 Unix 进程组和 Windows Job Object 回归测试共用。
#[cfg(any(unix, windows))]
fn make_tree_transport(directory: &Path) -> (LspTransport, std::path::PathBuf) {
    let marker = directory.join("tree-marker.txt");
    let child_pid = directory.join("child.pid");
    let (command, args, mut env) = peri_test_support::lsp_test_server("tree");
    env.insert(
        "PERI_LSP_TEST_PID".to_string(),
        directory.join("root.pid").to_string_lossy().into_owned(),
    );
    env.insert(
        "PERI_LSP_TEST_CHILD_PID".to_string(),
        child_pid.to_string_lossy().into_owned(),
    );
    env.insert(
        "PERI_LSP_TEST_TREE_MARKER".to_string(),
        marker.to_string_lossy().into_owned(),
    );
    (
        LspTransport::spawn(&command, &args, &env).expect("启动进程树测试 fixture 失败"),
        child_pid,
    )
}

/// close() 必须通过 Unix 独立进程组或 Windows Job Object 终止 LSP 的整个进程树。
#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_close_kills_lsp_process_tree() {
    let directory = tempfile::tempdir().expect("创建临时目录失败");
    let marker = directory.path().join("tree-marker.txt");
    let (transport, child_pid) = make_tree_transport(directory.path());
    wait_for_file(&child_pid).await;
    let (dispatcher, _rx) = MessageDispatcher::new(transport);

    dispatcher.close().await;
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(
        !marker.exists(),
        "close() 后进程树仍存活并写入 marker，不能只终止根进程"
    );
}

/// 丢弃 dispatcher 也必须触发整树清理，覆盖未显式调用 close() 的取消/错误路径。
#[cfg(any(unix, windows))]
#[tokio::test]
async fn test_drop_kills_lsp_process_tree() {
    let directory = tempfile::tempdir().expect("创建临时目录失败");
    let marker = directory.path().join("tree-marker.txt");
    let (transport, child_pid) = make_tree_transport(directory.path());
    wait_for_file(&child_pid).await;
    let (dispatcher, _rx) = MessageDispatcher::new(transport);

    drop(dispatcher);
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(
        !marker.exists(),
        "drop() 后进程树仍存活并写入 marker，未触发整树清理"
    );
}
