fn main() {
    // 生成桌面应用所需的平台清单和资源。
    tauri_build::build();

    // 原生验收启动的也是产品 bin，由 tauri_build 唯一嵌入 Windows manifest。
    // 不能因启用验收夹具再注入同一资源，否则 MSVC 会拒绝重复 manifest。

    // unit-test 是 lib target 的 harness，不能使用只匹配 [[test]] 的
    // rustc-link-arg-tests。这里只提供资源目录；lib.rs 的测试专用 link 属性
    // 负责接入资源，避免把同一 manifest 作为全局 linker 参数注入产品 bin。
    if std::env::var_os("CARGO_FEATURE_NATIVE_DESKTOP_TESTS").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
    {
        let out_dir = std::path::PathBuf::from(
            std::env::var_os("OUT_DIR").expect("OUT_DIR must be set for Tauri resources"),
        );
        println!("cargo:rustc-link-search=native={}", out_dir.display());
    }
}
