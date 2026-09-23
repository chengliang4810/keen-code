fn main() {
    // 生成桌面应用所需的平台清单和资源。
    tauri_build::build();

    // Tauri 默认只为产品二进制嵌入资源；交互测试同样需要 v6 公共控件清单，
    // 否则链接了真实窗口路径的测试会在 main 前因 TaskDialogIndirect 缺失而退出。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var_os("CARGO_FEATURE_NATIVE_DESKTOP_TESTS").is_some()
    {
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
}
