use std::{env, path::PathBuf, process::Command};

/// 为测试依赖编译不依赖外部脚本运行时的 Rust LSP 伪服务器。
fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest_dir.join("../test-support/lsp_test_server.rs");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let executable_name = if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        "peri_lsp_test_server.exe"
    } else {
        "peri_lsp_test_server"
    };
    let executable = out_dir.join(executable_name);
    let rustc = env::var_os("RUSTC").unwrap();
    let target = env::var("TARGET").unwrap();

    let status = Command::new(rustc)
        .args([
            "--crate-name",
            "peri_lsp_test_server",
            "--edition",
            "2021",
            "--target",
            &target,
        ])
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .status()
        .unwrap_or_else(|error| panic!("编译 Rust LSP 测试 fixture 失败: {error}"));
    assert!(
        status.success(),
        "Rust LSP 测试 fixture 编译器退出失败: {status}"
    );

    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!(
        "cargo:rustc-env=PERI_TEST_LSP_SERVER={}",
        executable.display()
    );
}
