//! 读取桌面系统代理，供不经过 WebView 的 Rust HTTP 与 Git 网络请求复用。

use std::env;

/// 返回当前 HTTPS 请求可使用的代理 URL。
///
/// 显式进程环境优先；桌面应用没有继承 shell 环境时，再读取 macOS/Windows
/// 系统网络设置。只返回地址，不记录日志，避免代理凭据进入诊断文件。
pub(crate) fn http_proxy_url() -> Option<String> {
    environment_proxy().or_else(platform_proxy)
}

fn environment_proxy() -> Option<String> {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .into_iter()
    .find_map(|key| env::var(key).ok().and_then(|value| normalize_proxy(&value)))
}

fn normalize_proxy(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(if value.contains("://") {
        value.to_owned()
    } else {
        format!("http://{value}")
    })
}

#[cfg(target_os = "macos")]
fn platform_proxy() -> Option<String> {
    let output = std::process::Command::new("scutil")
        .arg("--proxy")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then_some(())
        .and_then(|_| parse_macos_proxy(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(target_os = "macos")]
fn parse_macos_proxy(output: &str) -> Option<String> {
    let value = |key: &str| {
        output.lines().find_map(|line| {
            let (name, value) = line.trim().split_once(" : ")?;
            (name == key).then(|| value.trim())
        })
    };
    for prefix in ["HTTPS", "HTTP"] {
        if value(&format!("{prefix}Enable")) != Some("1") {
            continue;
        }
        let host = value(&format!("{prefix}Proxy"))?;
        let port = value(&format!("{prefix}Port"))?.parse::<u16>().ok()?;
        if !host.is_empty() && port > 0 {
            return Some(format!("http://{host}:{port}"));
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn platform_proxy() -> Option<String> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};

    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let settings = current_user
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        .ok()?;
    let enabled = settings.get_value::<u32, _>("ProxyEnable").ok()?;
    if enabled == 0 {
        return None;
    }
    let server = settings.get_value::<String, _>("ProxyServer").ok()?;
    parse_windows_proxy(&server)
}

#[cfg(target_os = "windows")]
fn parse_windows_proxy(value: &str) -> Option<String> {
    let entries = value
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    let protocol = |name: &str| {
        entries.iter().find_map(|entry| {
            let (protocol, address) = entry.split_once('=')?;
            protocol.eq_ignore_ascii_case(name).then_some(address)
        })
    };
    let selected = protocol("https")
        .or_else(|| protocol("http"))
        .or_else(|| entries.iter().find(|entry| !entry.contains('=')).copied())?;
    normalize_proxy(selected)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_proxy() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::normalize_proxy;

    #[test]
    fn normalizes_proxy_urls_without_changing_explicit_schemes() {
        assert_eq!(
            normalize_proxy("127.0.0.1:7890").as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert_eq!(
            normalize_proxy("socks5h://127.0.0.1:7890").as_deref(),
            Some("socks5h://127.0.0.1:7890")
        );
        assert_eq!(normalize_proxy("  "), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_enabled_macos_https_proxy() {
        let output = r#"<dictionary> {
  HTTPEnable : 1
  HTTPPort : 8080
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 1
  HTTPSPort : 9999
  HTTPSProxy : 127.0.0.1
}"#;
        assert_eq!(
            super::parse_macos_proxy(output).as_deref(),
            Some("http://127.0.0.1:9999")
        );
    }
}
