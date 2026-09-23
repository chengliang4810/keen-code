//! 开发专用评测进程；不包含在桌面默认发行物中。
#[tokio::main]
async fn main() {
    if let Err(error) = keencode_desktop::agent_runtime::benchmark::run().await {
        eprintln!("{error:#}");
        std::process::exit(
            if error.is::<keencode_desktop::agent_runtime::benchmark::BenchmarkTimeout>() {
                124
            } else {
                1
            },
        );
    }
}
