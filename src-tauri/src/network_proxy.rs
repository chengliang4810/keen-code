//! 读取桌面系统代理，供不经过 WebView 的 Rust HTTP 与 Git 网络请求复用。

use std::{
    env,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    time::Duration,
};

const PROXY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// 返回当前 HTTPS 请求可使用的代理 URL。
///
/// 显式进程环境优先；桌面应用没有继承 shell 环境时，再读取 macOS/Windows
/// 系统网络设置。只返回地址，不记录日志，避免代理凭据进入诊断文件。
pub(crate) fn http_proxy_url() -> Option<String> {
    environment_proxy().or_else(|| {
        let proxy = platform_proxy()?;
        proxy_supports_https(&proxy).then_some(proxy)
    })
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

/// 系统设置可能残留一个仍监听端口、但已无法转发 HTTPS 的本地代理。
/// 在注入 Git/reqwest 前用标准 CONNECT 做一次短探测，失败即沿用直连/镜像回退。
fn proxy_supports_https(proxy: &str) -> bool {
    let Ok(proxy) = url::Url::parse(proxy) else {
        return false;
    };
    if proxy.scheme() != "http" {
        return false;
    }
    let Some(host) = proxy.host_str() else {
        return false;
    };
    let Some(port) = proxy.port_or_known_default() else {
        return false;
    };
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return false;
    };
    for address in addresses {
        let Ok(mut stream) = TcpStream::connect_timeout(&address, PROXY_PROBE_TIMEOUT) else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(PROXY_PROBE_TIMEOUT));
        let _ = stream.set_write_timeout(Some(PROXY_PROBE_TIMEOUT));
        if stream
            .write_all(
                b"CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\nConnection: close\r\n\r\n",
            )
            .is_err()
        {
            continue;
        }
        let mut response = [0_u8; 256];
        let Ok(read) = stream.read(&mut response) else {
            continue;
        };
        if https_connect_succeeded(&response[..read]) {
            return true;
        }
    }
    false
}

fn https_connect_succeeded(response: &[u8]) -> bool {
    response.starts_with(b"HTTP/1.1 200 ") || response.starts_with(b"HTTP/1.0 200 ")
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
    use super::{https_connect_succeeded, normalize_proxy};

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
    fn accepts_only_successful_https_connect_responses() {
        assert!(https_connect_succeeded(
            b"HTTP/1.1 200 Connection established\r\n\r\n"
        ));
        assert!(!https_connect_succeeded(
            b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n"
        ));
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
