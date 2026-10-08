use std::{path::Path, process::Output, time::Duration};

// 复用 Shell 工具的进程组守卫，超时和正常退出均收割 Git 的所有后代。
pub(crate) fn run_git_with_timeout(root: &Path, args: &[&str]) -> Result<Output, String> {
    let request =
        rcode_tools::BoundedCommandRequest::new("git", root, Duration::from_secs(60), 1024 * 1024)
            .with_args(
                std::iter::once("-C")
                    .map(std::ffi::OsString::from)
                    .chain(std::iter::once(root.as_os_str().to_owned()))
                    .chain(args.iter().map(std::ffi::OsString::from))
                    .collect(),
            );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let result = runtime
        .block_on(rcode_tools::run_bounded_command(request))
        .map_err(|e| e.to_string())?;
    Ok(Output {
        status: result.status,
        stdout: result.stdout,
        stderr: result.stderr,
    })
}
