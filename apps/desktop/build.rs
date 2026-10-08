fn main() {
    configure_sidecar();
    let mut attributes = tauri_build::Attributes::new();
    if std::env::var("PROFILE").as_deref() != Ok("release") {
        // Tauri 的静态 CRT 占位库会污染 workspace 文档测试的链接搜索路径。
        // 开发构建使用系统 CRT；发布构建继续遵守 Tauri 的静态链接配置。
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new().static_vc_runtime(false));
    }
    tauri_build::try_build(attributes).expect("build RCode desktop resources");
    if std::env::var("TARGET").is_ok_and(|target| target.ends_with("windows-msvc")) {
        let output = std::env::var("OUT_DIR").expect("build output directory");
        println!("cargo:rustc-link-search=native={output}");
    }
}

fn configure_sidecar() {
    let Ok(target) = std::env::var("TARGET") else {
        return;
    };
    let extension = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let path = std::path::PathBuf::from("binaries").join(format!("rcode-cli-{target}{extension}"));
    let valid =
        std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0);
    if valid {
        return;
    }
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        panic!(
            "release sidecar {} is missing or empty; run pnpm build:cli before packaging",
            path.display()
        );
    }

    let mut config = std::env::var("TAURI_CONFIG")
        .map(|value| serde_json::from_str(&value).expect("parse TAURI_CONFIG"))
        .unwrap_or_else(|_| serde_json::json!({}));
    config["bundle"]["externalBin"] = serde_json::json!([]);
    std::env::set_var(
        "TAURI_CONFIG",
        serde_json::to_string(&config).expect("serialize TAURI_CONFIG"),
    );
}
