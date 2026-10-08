// Modified for RCode. See NOTICE.
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;

const HEADER_BLOCKLIST: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "transfer-encoding",
    "upgrade",
    "trailer",
    "expect",
];

pub(crate) use rcode_runtime::network::agent_http_client;
use rcode_runtime::network::{classify_and_collect_safe_ips, validate_url};
#[cfg(test)]
use rcode_runtime::network::{ip_kind, is_blocked_host_name, IpKind};

fn sanitize_headers(headers: Option<HashMap<String, String>>) -> Result<HeaderMap, String> {
    let mut map = HeaderMap::new();
    let Some(h) = headers else { return Ok(map) };
    for (k, v) in h {
        let lower = k.to_ascii_lowercase();
        if HEADER_BLOCKLIST.contains(&lower.as_str()) {
            return Err(format!("header not allowed: {k}"));
        }
        // CRLF injection: header value must not contain CR / LF / NUL.
        if v.as_bytes().iter().any(|b| matches!(b, 0 | b'\r' | b'\n')) {
            return Err(format!("header value contains control bytes: {k}"));
        }
        let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| e.to_string())?;
        let value = HeaderValue::from_str(&v).map_err(|e| e.to_string())?;
        map.insert(name, value);
    }
    Ok(map)
}

#[tauri::command]
pub async fn lm_ping(base_url: String) -> Result<u16, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("empty base url".into());
    }
    let probe = format!("{trimmed}/models");
    let parsed = validate_url(&probe, true)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "missing host".to_string())?
        .to_string();
    let safe_ips = classify_and_collect_safe_ips(&host, true).await?;

    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none());
    let addrs: Vec<SocketAddr> = safe_ips.iter().map(|ip| SocketAddr::new(*ip, 0)).collect();
    builder = builder.resolve_to_addrs(&host, &addrs);
    let client = builder.build().map_err(|e| e.to_string())?;
    client
        .get(parsed)
        .send()
        .await
        .map(|r| r.status().as_u16())
        .map_err(|e| e.to_string())
}
// AI HTTP proxy — bypasses webview CORS / Mixed-Content / PNA so local-network
// model servers (LM Studio, Ollama, vLLM) work in the production bundle.

#[derive(Debug, Serialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

struct CheckedResolver {
    allow_private: bool,
}

impl reqwest::dns::Resolve for CheckedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        let allow_private = self.allow_private;
        Box::pin(async move {
            let ips = tokio::time::timeout(
                Duration::from_secs(10),
                classify_and_collect_safe_ips(&host, allow_private),
            )
            .await
            .map_err(|_| std::io::Error::other("DNS query timed out"))?
            .map_err(std::io::Error::other)?;
            let addrs: reqwest::dns::Addrs =
                Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

fn build_safe_client(
    allow_private: bool,
    pinned: &[(String, Vec<IpAddr>)],
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .dns_resolver(Arc::new(CheckedResolver { allow_private }));
    // Pin reqwest's resolver to the IPs we just classified. Without this,
    // reqwest's own DNS lookup could return a different (private/metadata) IP
    // for the same hostname between classify and connect — classic DNS
    // rebinding attack. We pin port 0 because reqwest fills in the actual
    // port from the URL when wiring up the override map.
    for (host, ips) in pinned {
        let addrs: Vec<SocketAddr> = ips.iter().map(|ip| SocketAddr::new(*ip, 0)).collect();
        if !addrs.is_empty() {
            builder = builder.resolve_to_addrs(host.trim_matches(['[', ']']), &addrs);
        }
    }
    builder
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())
}

fn redirect_target(response: &reqwest::Response) -> Option<reqwest::Url> {
    if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    response
        .headers()
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()
        .and_then(|location| response.url().join(location).ok())
}

fn redirect_request(
    status: u16,
    previous: &reqwest::Url,
    next: &reqwest::Url,
    method: &mut Method,
    headers: &mut HeaderMap,
    body: &mut Option<Bytes>,
) {
    if matches!(status, 301..=303) && *method != Method::GET && *method != Method::HEAD {
        *method = Method::GET;
        *body = None;
        for name in [
            "content-type",
            "content-length",
            "content-encoding",
            "transfer-encoding",
        ] {
            headers.remove(name);
        }
    }
    if previous.origin() != next.origin() {
        let sensitive: Vec<_> = headers
            .iter()
            .filter(|(name, value)| {
                value.is_sensitive()
                    || matches!(
                        name.as_str(),
                        "authorization"
                            | "cookie"
                            | "cookie2"
                            | "proxy-authorization"
                            | "www-authenticate"
                            | "x-api-key"
                            | "api-key"
                            | "x-goog-api-key"
                    )
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in sensitive {
            headers.remove(name);
        }
    }
}

async fn send_safe_request(
    url: &str,
    method: &str,
    headers: Option<HashMap<String, String>>,
    body: Option<Vec<u8>>,
    allow_private: bool,
) -> Result<reqwest::Response, String> {
    let mut url = validate_url(url, allow_private)?;
    let host = url.host_str().ok_or("missing host")?.to_owned();
    let safe_ips = tokio::time::timeout(
        Duration::from_secs(10),
        classify_and_collect_safe_ips(&host, allow_private),
    )
    .await
    .map_err(|_| "DNS query timed out")??;
    let client = build_safe_client(allow_private, &[(host, safe_ips)])?;
    let mut method = Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let mut headers = sanitize_headers(headers)?;
    let mut body = body.map(Bytes::from);
    for redirects in 0..=10 {
        let mut request = client
            .request(method.clone(), url.clone())
            .headers(headers.clone());
        if let Some(body) = &body {
            request = request.body(body.clone());
        }
        let response = request
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        let Some(next) = redirect_target(&response) else {
            return Ok(response);
        };
        if redirects == 10 {
            return Err("too many redirects".into());
        }
        let next = validate_url(next.as_str(), allow_private)?;
        redirect_request(
            response.status().as_u16(),
            &url,
            &next,
            &mut method,
            &mut headers,
            &mut body,
        );
        url = next;
    }
    unreachable!("redirect loop returns at its limit")
}

fn header_map_to_strings(headers: &HeaderMap) -> HashMap<String, String> {
    let mut out = HashMap::with_capacity(headers.len());
    for (k, v) in headers {
        if let Ok(s) = v.to_str() {
            out.insert(k.as_str().to_ascii_lowercase(), s.to_string());
        }
    }
    out
}

#[tauri::command]
pub async fn ai_http_request(
    url: String,
    method: String,
    headers: Option<HashMap<String, String>>,
    body: Option<Vec<u8>>,
    allow_private_network: Option<bool>,
) -> Result<HttpResponse, String> {
    let allow_private = allow_private_network.unwrap_or(false);
    let resp = send_safe_request(&url, &method, headers, body, allow_private).await?;

    let status = resp.status().as_u16();
    let headers = header_map_to_strings(resp.headers());
    let body = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AiStreamEvent {
    Headers {
        status: u16,
        headers: HashMap<String, String>,
    },
    Chunk {
        bytes: Vec<u8>,
    },
    End,
    Error {
        message: String,
    },
}

#[tauri::command]
pub async fn ai_http_stream(
    url: String,
    method: String,
    headers: Option<HashMap<String, String>>,
    body: Option<Vec<u8>>,
    allow_private_network: Option<bool>,
    on_event: Channel<AiStreamEvent>,
) -> Result<(), String> {
    let allow_private = allow_private_network.unwrap_or(false);
    let resp = match send_safe_request(&url, &method, headers, body, allow_private).await {
        Ok(r) => r,
        Err(e) => {
            let _ = on_event.send(AiStreamEvent::Error { message: e.clone() });
            return Err(e);
        }
    };

    let status = resp.status().as_u16();
    let headers = header_map_to_strings(resp.headers());
    let _ = on_event.send(AiStreamEvent::Headers { status, headers });

    let mut stream = resp.bytes_stream();
    while let Some(item) = stream.next().await {
        match item {
            Ok(chunk) => {
                let bytes: Bytes = chunk;
                if on_event
                    .send(AiStreamEvent::Chunk {
                        bytes: bytes.to_vec(),
                    })
                    .is_err()
                {
                    // Channel dropped (frontend aborted) — stop streaming.
                    return Ok(());
                }
            }
            Err(e) => {
                let _ = on_event.send(AiStreamEvent::Error {
                    message: e.to_string(),
                });
                return Err(e.to_string());
            }
        }
    }

    let _ = on_event.send(AiStreamEvent::End);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn http_server(
        responses: impl FnOnce(SocketAddr) -> Vec<String>,
    ) -> (SocketAddr, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let responses = responses(address);
        let thread = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "local request timed out"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(e) => panic!("local listener: {e}"),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let read = socket.read(&mut buffer).unwrap();
                    assert!(read > 0, "incomplete request");
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                        let length: usize = header
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(bytes).unwrap());
                socket.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        (address, thread)
    }

    #[test]
    fn mapped_ipv6_inherits_ipv4_security_classification() {
        for (ip, expected) in [
            ("::ffff:169.254.169.254", IpKind::BlockedMetadata),
            ("::ffff:127.0.0.1", IpKind::Loopback),
            ("::ffff:10.0.0.1", IpKind::Private),
            ("::ffff:192.168.1.1", IpKind::Private),
            ("::ffff:8.8.8.8", IpKind::Public),
        ] {
            assert_eq!(ip_kind(ip.parse().unwrap()), expected, "{ip}");
        }
    }

    #[test]
    fn url_literals_and_metadata_aliases_share_address_policy() {
        for url in [
            "http://[::ffff:169.254.169.254]/",
            "http://[fd00:ec2::254]/",
            "http://metadata.google.internal./",
        ] {
            assert!(validate_url(url, true).is_err(), "{url}");
        }
        for url in ["http://[::1]:1234/", "http://[::ffff:127.0.0.1]/"] {
            assert!(validate_url(url, false).is_err());
            assert!(validate_url(url, true).is_ok());
        }
    }

    #[tokio::test]
    async fn checked_resolver_rejects_redirect_host_dns_without_private_opt_in() {
        use reqwest::dns::Resolve;
        let resolver = CheckedResolver {
            allow_private: false,
        };
        assert!(resolver
            .resolve("localhost".parse().unwrap())
            .await
            .is_err());
        let resolver = CheckedResolver {
            allow_private: true,
        };
        assert!(resolver
            .resolve("metadata.google.internal".parse().unwrap())
            .await
            .is_err());
        let addresses = resolver
            .resolve("localhost".parse().unwrap())
            .await
            .unwrap()
            .collect::<Vec<_>>();
        assert!(!addresses.is_empty());
        assert!(addresses
            .iter()
            .all(|address| ip_kind(address.ip()) == IpKind::Loopback));
    }

    #[tokio::test]
    async fn local_redirect_preserves_post_body_and_strips_cross_origin_credentials() {
        let (address, server) = http_server(|address| {
            vec![
            format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://localhost:{}/target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", address.port()),
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_owned(),
        ]
        });
        let headers = Some(HashMap::from([
            ("authorization".to_owned(), "Bearer synthetic".to_owned()),
            ("x-api-key".to_owned(), "synthetic".to_owned()),
            ("content-type".to_owned(), "application/json".to_owned()),
        ]));
        let response = send_safe_request(
            &format!("http://{address}/start"),
            "POST",
            headers,
            Some(b"payload".to_vec()),
            true,
        )
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(response.text().await.unwrap(), "ok");
        let requests = server.join().unwrap();
        assert!(requests[0].contains("Bearer synthetic"));
        assert!(requests[1].starts_with("POST /target "));
        assert!(requests[1].ends_with("payload"));
        assert!(!requests[1].to_ascii_lowercase().contains("authorization:"));
        assert!(!requests[1].to_ascii_lowercase().contains("x-api-key:"));
        assert!(requests[1].contains("application/json"));
    }

    #[tokio::test]
    async fn same_origin_303_redirect_retains_authentication_and_switches_to_get() {
        let (address, server) = http_server(|_| {
            vec![
            "HTTP/1.1 303 See Other\r\nLocation: /target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_owned(),
        ]
        });
        let response = send_safe_request(
            &format!("http://{address}/start"),
            "POST",
            Some(HashMap::from([
                ("x-api-key".to_owned(), "synthetic".to_owned()),
                ("content-type".to_owned(), "application/json".to_owned()),
            ])),
            Some(b"payload".to_vec()),
            true,
        )
        .await
        .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let requests = server.join().unwrap();
        assert!(requests[1].starts_with("GET /target "));
        assert!(requests[1].contains("x-api-key: synthetic"));
        assert!(!requests[1].contains("payload"));
        assert!(!requests[1].contains("content-type:"));
    }

    #[tokio::test]
    async fn metadata_redirect_is_rejected_before_any_second_request() {
        for target in [
            "http://metadata.google.internal/",
            "http://[::ffff:169.254.169.254]/",
        ] {
            let (address, server) = http_server(|_| {
                vec![format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")]
            });
            let result =
                send_safe_request(&format!("http://{address}/start"), "GET", None, None, true)
                    .await;
            assert!(result.unwrap_err().contains("host not allowed"));
            assert_eq!(server.join().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn redirects_remain_bounded() {
        let (address, server) = http_server(|_| {
            vec![
            "HTTP/1.1 302 Found\r\nLocation: /start\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(); 11
        ]
        });
        let result =
            send_safe_request(&format!("http://{address}/start"), "GET", None, None, true).await;
        assert_eq!(result.unwrap_err(), "too many redirects");
        assert_eq!(server.join().unwrap().len(), 11);
    }

    #[test]
    fn metadata_ips_classified_as_blocked() {
        // AWS / Google / Azure all share the IPv4 169.254.169.254 link-local.
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))),
            IpKind::BlockedMetadata
        );
        // AWS IPv6 metadata
        assert_eq!(
            ip_kind("fd00:ec2::254".parse().unwrap()),
            IpKind::BlockedMetadata
        );
        // Any link-local IPv4 (169.254/16) — same network range, still blocked.
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1))),
            IpKind::BlockedMetadata
        );
        // IPv6 link-local fe80::/10
        assert_eq!(ip_kind("fe80::1".parse().unwrap()), IpKind::BlockedMetadata);
    }

    #[test]
    fn private_ips_classified_correctly() {
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
            IpKind::Private
        );
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))),
            IpKind::Private
        );
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))),
            IpKind::Private
        );
        // CGNAT 100.64/10
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1))),
            IpKind::Private
        );
    }

    #[test]
    fn loopback_classified_as_loopback() {
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
            IpKind::Loopback
        );
        assert_eq!(ip_kind("::1".parse().unwrap()), IpKind::Loopback);
    }

    #[test]
    fn public_ips_classified_as_public() {
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))),
            IpKind::Public
        );
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))),
            IpKind::Public
        );
    }

    #[test]
    fn validate_url_blocks_userinfo_and_metadata_hostnames() {
        // URLs with userinfo can confuse browsers / leak creds in redirects.
        assert!(validate_url("http://user:pass@example.com/", true).is_err());
        // Cloud metadata-by-name.
        assert!(validate_url("http://metadata.google.internal/", true).is_err());
        assert!(validate_url("http://metadata/", true).is_err());
        assert!(validate_url("http://metadata.azure.com/", true).is_err());
    }

    #[test]
    fn validate_url_rejects_non_http_schemes() {
        assert!(validate_url("ftp://example.com/", true).is_err());
        assert!(validate_url("file:///etc/passwd", true).is_err());
        assert!(validate_url("javascript:alert(1)", true).is_err());
    }

    #[test]
    fn sanitize_headers_blocks_crlf_injection() {
        let mut h = HashMap::new();
        h.insert("X-Foo".to_string(), "bar\r\nX-Evil: yes".to_string());
        assert!(sanitize_headers(Some(h)).is_err());
    }

    #[test]
    fn sanitize_headers_blocks_hop_by_hop_headers() {
        for hop in [
            "host",
            "content-length",
            "connection",
            "proxy-authorization",
        ] {
            let mut h = HashMap::new();
            h.insert(hop.to_string(), "value".to_string());
            assert!(
                sanitize_headers(Some(h)).is_err(),
                "expected {hop} to be rejected"
            );
        }
    }

    #[test]
    fn special_ipv4_ranges_do_not_classify_as_public() {
        assert_eq!(ip_kind(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), IpKind::Loopback);
        assert_eq!(ip_kind(IpAddr::V4(Ipv4Addr::BROADCAST)), IpKind::Loopback);
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1))),
            IpKind::Loopback
        );
    }

    #[test]
    fn private_range_boundaries_hold_on_ipv4() {
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))),
            IpKind::Private
        );
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(172, 32, 0, 1))),
            IpKind::Public
        );
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(100, 127, 255, 255))),
            IpKind::Private
        );
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 1))),
            IpKind::Public
        );
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(198, 18, 0, 1))),
            IpKind::Private
        );
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(198, 19, 255, 255))),
            IpKind::Private
        );
        assert_eq!(
            ip_kind(IpAddr::V4(Ipv4Addr::new(198, 20, 0, 1))),
            IpKind::Public
        );
    }

    #[test]
    fn ipv6_unique_local_and_multicast_are_not_public() {
        assert_eq!(ip_kind("fd12:3456::1".parse().unwrap()), IpKind::Private);
        assert_eq!(ip_kind("fc00::1".parse().unwrap()), IpKind::Private);
        assert_eq!(ip_kind("ff02::1".parse().unwrap()), IpKind::Loopback);
        assert_eq!(ip_kind("::".parse().unwrap()), IpKind::Loopback);
        assert_eq!(ip_kind("2606:4700::1111".parse().unwrap()), IpKind::Public);
    }

    #[test]
    fn blocked_host_name_match_is_case_insensitive_and_exact() {
        assert!(is_blocked_host_name("METADATA.google.internal"));
        assert!(!is_blocked_host_name("metadata.evil.internal"));
        assert!(!is_blocked_host_name("my-metadata"));
    }

    #[test]
    fn validate_url_requires_a_host() {
        // A URL that parses but carries no host must not slip through.
        assert!(validate_url("http://", true).is_err());
    }
}
