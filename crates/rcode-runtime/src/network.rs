// Modified for RCode. See NOTICE.
use std::net::IpAddr;

pub fn is_blocked_host_name(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    matches!(
        host.as_str(),
        "metadata.google.internal" | "metadata" | "metadata.azure.com"
    )
}

pub fn ip_kind(ip: IpAddr) -> IpKind {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            // Cloud metadata IPv4: 169.254.169.254
            if v.is_link_local() {
                return IpKind::BlockedMetadata;
            }
            if v.is_loopback() || v.is_unspecified() || v.is_broadcast() || v.is_multicast() {
                return IpKind::Loopback;
            }
            // RFC1918 + CGNAT + benchmarking + IETF
            if o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 100 && (64..=127).contains(&o[1]))
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
            {
                return IpKind::Private;
            }
            IpKind::Public
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return ip_kind(IpAddr::V4(v4));
            }
            if v.is_loopback() || v.is_unspecified() || v.is_multicast() {
                return IpKind::Loopback;
            }
            // Cloud metadata IPv6 (AWS): fd00:ec2::254
            let segs = v.segments();
            if segs[0] == 0xfd00 && segs[1] == 0xec2 {
                return IpKind::BlockedMetadata;
            }
            // fe80::/10 link-local
            if segs[0] & 0xffc0 == 0xfe80 {
                return IpKind::BlockedMetadata;
            }
            // fc00::/7 unique-local (private)
            if segs[0] & 0xfe00 == 0xfc00 {
                return IpKind::Private;
            }
            IpKind::Public
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum IpKind {
    Public,
    Private,
    Loopback,
    BlockedMetadata,
}

/// Resolve `host` once and return both its safety classification and the
/// concrete IPs we resolved. Callers can pin reqwest to these IPs to defeat
/// DNS rebinding (where a second lookup returns a different address).
async fn resolve_and_classify(host: &str) -> Result<(IpKind, Vec<IpAddr>), String> {
    let host = host.trim_matches(['[', ']']);
    if is_blocked_host_name(host) {
        return Err(format!("host not allowed: {host}"));
    }
    // Direct literal? Skip DNS.
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok((ip_kind(ip), vec![ip]));
    }
    let host_owned = host.to_string();
    let lookup = tokio::task::spawn_blocking(move || {
        (host_owned.as_str(), 0u16)
            .to_socket_addrs()
            .map(|it| it.map(|a| a.ip()).collect::<Vec<_>>())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("dns: {e}"))?;
    if lookup.is_empty() {
        return Err("dns: no addresses".into());
    }
    let mut worst = IpKind::Public;
    for ip in &lookup {
        let k = ip_kind(*ip);
        worst = match (worst, k) {
            (_, IpKind::BlockedMetadata) => IpKind::BlockedMetadata,
            (IpKind::BlockedMetadata, _) => IpKind::BlockedMetadata,
            (IpKind::Public, x) => x,
            (x, IpKind::Public) => x,
            (a, _) => a,
        };
    }
    Ok((worst, lookup))
}

use std::net::ToSocketAddrs;

pub fn validate_url(url: &str, allow_private: bool) -> Result<reqwest::Url, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid url: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        s => return Err(format!("scheme not allowed: {s}")),
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err("userinfo in url is not allowed".into());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "missing host".to_string())?;
    if is_blocked_host_name(host) {
        return Err(format!("host not allowed: {host}"));
    }
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        validate_ip_kind(ip_kind(ip), host, allow_private)?;
    }
    Ok(parsed)
}

fn validate_ip_kind(kind: IpKind, host: &str, allow_private: bool) -> Result<(), String> {
    match kind {
        IpKind::BlockedMetadata => Err(format!("host not allowed: {host}")),
        IpKind::Loopback | IpKind::Private if !allow_private => Err(format!(
            "host {host} resolves to a private/loopback address; this endpoint requires explicit opt-in",
        )),
        _ => Ok(()),
    }
}

/// 原生 Agent 与 MCP 共用现有地址分类，并禁止重定向及代理再次解析目标。
pub async fn agent_http_client(url: &str, allow_private: bool) -> Result<reqwest::Client, String> {
    let parsed = validate_url(url, allow_private)?;
    let host = parsed
        .host_str()
        .ok_or("missing host")?
        .trim_matches(['[', ']']);
    let ips = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        classify_and_collect_safe_ips(host, allow_private),
    )
    .await
    .map_err(|_| "服务地址 DNS 查询超时")??;
    let addrs: Vec<_> = ips
        .into_iter()
        .map(|ip| std::net::SocketAddr::new(ip, parsed.port_or_known_default().unwrap_or(443)))
        .collect();
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addrs)
        .build()
        .map_err(|e| e.without_url().to_string())
}

/// Classify the host AND return safe IPs to pin reqwest's resolver to.
/// Defeats DNS rebinding (second-lookup-returns-different-IP) by reusing
/// exactly the addresses that passed `ip_kind`.
pub async fn classify_and_collect_safe_ips(
    host: &str,
    allow_private: bool,
) -> Result<Vec<IpAddr>, String> {
    let (worst, ips) = resolve_and_classify(host).await?;
    validate_ip_kind(worst, host, allow_private)?;
    let safe: Vec<IpAddr> = ips
        .into_iter()
        .filter(|ip| match ip_kind(*ip) {
            IpKind::BlockedMetadata => false,
            IpKind::Loopback | IpKind::Private => allow_private,
            IpKind::Public => true,
        })
        .collect();
    if safe.is_empty() {
        return Err(format!("host {host}: no safe IPs"));
    }
    Ok(safe)
}

pub fn validate_network_endpoint(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|_| "服务地址格式无效")?;
    if value.len() > 8192
        || !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("服务地址只允许无凭据的 HTTP/HTTPS URL".into());
    }
    let host = url.host_str().ok_or("服务地址缺少主机")?;
    if matches!(
        host.to_ascii_lowercase().as_str(),
        "metadata" | "metadata.google.internal" | "metadata.azure.com"
    ) || host
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .is_ok_and(|ip| match ip {
            IpAddr::V4(ip) => ip.is_link_local() || ip.is_unspecified() || ip.is_multicast(),
            IpAddr::V6(ip) => {
                ip.is_unicast_link_local()
                    || ip.is_unspecified()
                    || ip.is_multicast()
                    || ip.segments()[0..2] == [0xfd00, 0xec2]
            }
        })
    {
        return Err("拒绝访问云元数据或无效服务地址".into());
    }
    Ok(())
}
