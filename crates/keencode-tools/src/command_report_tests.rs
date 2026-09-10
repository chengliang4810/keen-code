//! 命令报告的字节预算、完整日志保留和字符边界回归测试。

use super::*;
use std::pin::Pin;
use std::task::{Context, Poll};
use tempfile::tempdir;
use tokio::io::ReadBuf;

fn spec(directory: &Path) -> ProcessSpec {
    ProcessSpec {
        label: "Bash",
        programs: Vec::new(),
        args: Vec::new(),
        cwd: directory.to_path_buf(),
        timeout: Duration::from_secs(1),
        environment: Vec::new(),
    }
}

#[tokio::test]
async fn complete_utf8_preview_is_decoded_across_buffer_boundary() {
    let directory = tempdir().unwrap();
    let input = "ab中文";
    let stdout = capture_stream(
        input.as_bytes(),
        directory.path().to_path_buf(),
        "stdout",
        input.len(),
    )
    .await;
    assert_eq!(stdout.preview, input);
    assert!(
        stdout.artifact_path.as_ref().unwrap().exists(),
        "最终报告确定前不能删除副本"
    );
    let stderr = capture_stream(&b""[..], directory.path().to_path_buf(), "stderr", 16).await;
    let report = render_process_report(
        &spec(directory.path()),
        &ProcessTermination::TimedOut,
        stdout,
        stderr,
    )
    .await;
    assert!(report.contains(input));
    assert!(!report.contains('\u{fffd}'));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    assert!(
        !render_preview(
            "中文".as_bytes().get(..5).unwrap(),
            &"中文".as_bytes()[1..],
            true,
            100
        )
        .contains('\u{fffd}')
    );
}

#[tokio::test]
async fn timeout_and_cancellation_keep_bounded_diagnostics_and_spool_failures() {
    for termination in [ProcessTermination::TimedOut, ProcessTermination::Cancelled] {
        let directory = tempdir().unwrap();
        let blocked = directory.path().join("not-a-directory");
        std::fs::write(&blocked, "occupied").unwrap();
        let input = format!("START\n{}\nEND", "错误".repeat(2_000));
        let stdout = capture_stream(input.as_bytes(), blocked.clone(), "stdout", 16 * 1024).await;
        let stderr = capture_stream(&b""[..], blocked, "stderr", 16 * 1024).await;
        let report =
            render_process_report(&spec(directory.path()), &termination, stdout, stderr).await;
        assert!(report.len() <= TOOL_OUTPUT_LIMITS.max_tool_error_message_bytes);
        assert!(report.contains("START") && report.contains("END"));
        assert!(report.contains("输出落盘警告"));
        assert!(!report.contains("完整输出："));
        assert!(!report.contains('\u{fffd}'));
        match termination {
            ProcessTermination::TimedOut => assert!(report.contains("执行超时")),
            ProcessTermination::Cancelled => assert!(report.contains("已取消")),
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn oversized_metadata_is_bounded_without_inventing_shortened_paths() {
    let make_stream = || CapturedStream {
        preview: "前后".repeat(3_000),
        total_bytes: 18_000,
        truncated: true,
        artifact_path: Some(PathBuf::from(format!("/{}", "long-path".repeat(1_000)))),
        artifact_error: Some("保存失败".repeat(1_000)),
    };
    let report = render_process_report(
        &spec(Path::new(&"long-directory".repeat(1_000))),
        &ProcessTermination::Cancelled,
        make_stream(),
        make_stream(),
    )
    .await;
    assert!(report.len() <= TOOL_OUTPUT_LIMITS.max_tool_error_message_bytes);
    assert!(report.contains("输出文件路径超出报告预算，已省略"));
    assert!(report.contains("输出落盘警告"));
    assert!(!report.contains("/long-path"));
}

struct FailingReader(bool);

impl AsyncRead for FailingReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.0 {
            Poll::Ready(Err(io::Error::other("测试管道中断")))
        } else {
            self.0 = true;
            buf.put_slice(b"partial-output");
            Poll::Ready(Ok(()))
        }
    }
}

#[tokio::test]
async fn interrupted_capture_does_not_claim_partial_artifact_is_complete() {
    let directory = tempdir().unwrap();
    let stdout = capture_stream(
        FailingReader(false),
        directory.path().to_path_buf(),
        "stdout",
        16 * 1024,
    )
    .await;
    let path = stdout.artifact_path.clone().unwrap();
    let stderr = capture_stream(&b""[..], directory.path().to_path_buf(), "stderr", 16).await;
    let report = render_process_report(
        &spec(directory.path()),
        &ProcessTermination::Cancelled,
        stdout,
        stderr,
    )
    .await;
    assert!(report.contains("stdout 输出文件（可能不完整）："));
    assert!(report.contains("测试管道中断"));
    assert!(!report.contains("stdout 完整输出："));
    assert_eq!(std::fs::read(path).unwrap(), b"partial-output");
}
