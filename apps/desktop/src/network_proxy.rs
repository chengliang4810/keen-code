//! 将桌面系统代理应用到整个 KeenCode 进程及其子进程。

use std::{collections::HashSet, env};

const LOOPBACK_BYPASS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlatformProxy {
    http: Option<String>,
    https: Option<String>,
    bypass: Vec<String>,
}

/// 在 Tauri、异步运行时和其他线程创建前，把系统代理转成通用进程环境。
/// reqwest、Tauri Updater、Git/npm/MCP 子进程和内置终端随后共享该默认值。
pub(crate) fn configure_before_start() {
    if environment_proxy().is_some() {
        return;
    }
    // OS 系统代理缺失时（例如只在 git 里配了本地代理而未启用系统代理），回退使用 git
    // 全局代理，让 reqwest、Tauri Updater 与各子进程和 git 共用同一网络出口。
    let Some(proxy) = platform_proxy().or_else(git_global_proxy) else {
        return;
    };
    let no_proxy_configured = ["NO_PROXY", "no_proxy"]
        .into_iter()
        .any(|key| env::var_os(key).is_some_and(|value| !value.is_empty()));

    // SAFETY: main 在 Tauri、异步运行时和其他线程启动前调用本函数。
    unsafe {
        if let Some(http) = &proxy.http {
            env::set_var("HTTP_PROXY", http);
            env::set_var("http_proxy", http);
        }
        if let Some(https) = &proxy.https {
            env::set_var("HTTPS_PROXY", https);
            env::set_var("https_proxy", https);
        }
        if proxy.http == proxy.https
            && let Some(all) = &proxy.http
        {
            env::set_var("ALL_PROXY", all);
            env::set_var("all_proxy", all);
        }
        if !no_proxy_configured {
            let bypass = no_proxy_value(&proxy.bypass);
            env::set_var("NO_PROXY", &bypass);
            env::set_var("no_proxy", bypass);
        }
    }
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

/// 返回当前进程应当使用的代理地址（`https` 优先），供不会自动读取环境变量的客户端
/// （如 Tauri 更新器）显式套用；与 `configure_before_start` 的选择顺序保持一致。
pub(crate) fn effective_proxy() -> Option<String> {
    environment_proxy().or_else(|| {
        platform_proxy()
            .and_then(|proxy| proxy.https.or(proxy.http))
            .or_else(|| git_global_proxy().and_then(|proxy| proxy.https.or(proxy.http)))
    })
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

/// 读取 git 全局配置的代理，作为操作系统代理缺失时的回退来源。
///
/// 常见于只在 git 里配了本地代理（如 Clash 的 HTTP 端口）、却没有启用系统代理的开发机：
/// 此时市场克隆的 git 子进程能走代理，reqwest 与 Tauri Updater 却因没有环境变量而直连失败。
/// 复用同一份 git 代理，让全部网络出口保持一致。
fn git_global_proxy() -> Option<PlatformProxy> {
    let read = |key: &str| -> Option<String> {
        let output = std::process::Command::new("git")
            .args(["config", "--global", "--get", key])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        output.status.success().then_some(())?;
        String::from_utf8(output.stdout).ok()
    };
    platform_from_git_config(
        read("http.proxy").as_deref(),
        read("https.proxy").as_deref(),
    )
}

/// 依据 git 的 `http.proxy` / `https.proxy` 构造平台代理；与 git 自身一致，
/// 未单独配置 `https.proxy` 时 HTTPS 沿用 `http.proxy`。
fn platform_from_git_config(
    http_raw: Option<&str>,
    https_raw: Option<&str>,
) -> Option<PlatformProxy> {
    let http = http_raw.and_then(normalize_proxy);
    let https = https_raw.and_then(normalize_proxy).or_else(|| http.clone());
    if http.is_none() && https.is_none() {
        return None;
    }
    Some(PlatformProxy {
        http,
        https,
        bypass: Vec::new(),
    })
}

fn no_proxy_value(entries: &[String]) -> String {
    let mut seen = HashSet::new();
    LOOPBACK_BYPASS
        .iter()
        .copied()
        .chain(entries.iter().map(String::as_str))
        .filter_map(|value| {
            let value = value.trim();
            if value.is_empty() || value.eq_ignore_ascii_case("<local>") {
                return None;
            }
            let normalized = normalize_bypass(value);
            seen.insert(normalized.clone()).then_some(normalized)
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn normalize_bypass(value: &str) -> String {
    if let Some(domain) = value.strip_prefix("*.") {
        return format!(".{domain}");
    }
    let parts = value.split('.').collect::<Vec<_>>();
    let fixed = parts.iter().take_while(|part| **part != "*").count();
    if fixed > 0
        && fixed < 4
        && parts[fixed..].iter().all(|part| *part == "*")
        && parts[..fixed].iter().all(|part| part.parse::<u8>().is_ok())
    {
        let mut address = parts[..fixed].join(".");
        for _ in fixed..4 {
            address.push_str(".0");
        }
        return format!("{address}/{}", fixed * 8);
    }
    value.to_owned()
}

#[cfg(target_os = "macos")]
fn platform_proxy() -> Option<PlatformProxy> {
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

#[cfg(any(target_os = "macos", test))]
fn parse_macos_proxy(output: &str) -> Option<PlatformProxy> {
    let value = |key: &str| {
        output.lines().find_map(|line| {
            let (name, value) = line.trim().split_once(" : ")?;
            (name == key).then(|| value.trim())
        })
    };
    let proxy = |prefix: &str| {
        if value(&format!("{prefix}Enable")) != Some("1") {
            return None;
        }
        let host = value(&format!("{prefix}Proxy"))?;
        let port = value(&format!("{prefix}Port"))?.parse::<u16>().ok()?;
        if host.is_empty() || port == 0 {
            return None;
        }
        Some(format!("http://{host}:{port}"))
    };
    let http = proxy("HTTP");
    let https = proxy("HTTPS");
    (http.is_some() || https.is_some()).then(|| PlatformProxy {
        http,
        https,
        bypass: parse_macos_bypass(output),
    })
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos_bypass(output: &str) -> Vec<String> {
    let mut in_exceptions = false;
    let mut entries = Vec::new();
    for line in output.lines().map(str::trim) {
        if line.starts_with("ExceptionsList : <array>") {
            in_exceptions = true;
            continue;
        }
        if in_exceptions && line == "}" {
            break;
        }
        if in_exceptions
            && let Some((index, value)) = line.split_once(" : ")
            && index.parse::<usize>().is_ok()
        {
            entries.push(value.trim().to_owned());
        }
    }
    entries
}

#[cfg(target_os = "windows")]
fn platform_proxy() -> Option<PlatformProxy> {
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
    let bypass = settings
        .get_value::<String, _>("ProxyOverride")
        .unwrap_or_default();
    parse_windows_proxy(&server, &bypass)
}

#[cfg(any(target_os = "windows", test))]
fn parse_windows_proxy(value: &str, bypass: &str) -> Option<PlatformProxy> {
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
    let shared = entries.iter().find(|entry| !entry.contains('=')).copied();
    let http = protocol("http").or(shared).and_then(normalize_proxy);
    let https = protocol("https").or(shared).and_then(normalize_proxy);
    if http.is_none() && https.is_none() {
        return None;
    }
    Some(PlatformProxy {
        http,
        https,
        bypass: bypass
            .split(';')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_owned)
            .collect(),
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_proxy() -> Option<PlatformProxy> {
    None
}

#[cfg(test)]
mod tests {
    use super::{no_proxy_value, normalize_proxy, parse_windows_proxy, platform_from_git_config};

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

    #[test]
    fn preserves_system_bypass_rules_for_native_clients() {
        assert_eq!(
            no_proxy_value(&[
                "*.example.com".to_owned(),
                "10.0.0.0/8".to_owned(),
                "156.233.*".to_owned(),
                "<local>".to_owned(),
            ]),
            "localhost,127.0.0.1,::1,.example.com,10.0.0.0/8,156.233.0.0/16"
        );
    }

    #[test]
    fn parses_windows_protocol_proxy_and_bypass_list() {
        let proxy = parse_windows_proxy(
            "http=127.0.0.1:8080;https=127.0.0.1:8443",
            "localhost;*.example.com;<local>",
        )
        .expect("应解析 Windows 系统代理");
        assert_eq!(proxy.http.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(proxy.https.as_deref(), Some("http://127.0.0.1:8443"));
        assert_eq!(
            no_proxy_value(&proxy.bypass),
            "localhost,127.0.0.1,::1,.example.com"
        );
    }

    #[test]
    fn parses_enabled_macos_https_proxy() {
        let output = r#"<dictionary> {
  ExceptionsList : <array> {
    0 : 127.0.0.1
    1 : *.example.com
  }
  HTTPEnable : 1
  HTTPPort : 8080
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 1
  HTTPSPort : 9999
  HTTPSProxy : 127.0.0.1
}"#;
        let proxy = super::parse_macos_proxy(output).expect("应解析 macOS 系统代理");
        assert_eq!(proxy.http.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(proxy.https.as_deref(), Some("http://127.0.0.1:9999"));
        assert_eq!(
            no_proxy_value(&proxy.bypass),
            "localhost,127.0.0.1,::1,.example.com"
        );
    }

    #[test]
    fn git_http_proxy_also_covers_https() {
        // 只配 http.proxy 时，https 沿用同一地址（与 git 行为一致）。
        let proxy =
            platform_from_git_config(Some("127.0.0.1:7890"), None).expect("应构造 git 代理");
        assert_eq!(proxy.http.as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(proxy.https.as_deref(), Some("http://127.0.0.1:7890"));
    }

    #[test]
    fn git_keeps_explicit_https_proxy_scheme() {
        let proxy = platform_from_git_config(Some("http://host:1"), Some("socks5h://host:2"))
            .expect("应保留各自协议");
        assert_eq!(proxy.http.as_deref(), Some("http://host:1"));
        assert_eq!(proxy.https.as_deref(), Some("socks5h://host:2"));
    }

    #[test]
    fn missing_git_proxy_yields_none() {
        assert!(platform_from_git_config(None, None).is_none());
        assert!(platform_from_git_config(Some("   "), None).is_none());
    }
}
