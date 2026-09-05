//! peri workspace 测试辅助：提供不依赖外部脚本运行时的 Rust LSP fixture。

use std::{collections::HashMap, path::PathBuf};

/// 构造测试伪服务器的命令、模式参数和启用标记环境变量。
pub fn lsp_test_server(mode: &str) -> (String, Vec<String>, HashMap<String, String>) {
    (
        lsp_test_server_path().to_string_lossy().into_owned(),
        vec![mode.to_string()],
        HashMap::from([("PERI_LSP_TEST_SERVER".to_string(), "1".to_string())]),
    )
}

/// 返回 build script 编译的 Rust LSP 测试 fixture 路径。
fn lsp_test_server_path() -> PathBuf {
    PathBuf::from(env!("PERI_TEST_LSP_SERVER"))
}
