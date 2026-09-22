use futures_util::{SinkExt, StreamExt};
use keencode_web::{
    CSRF_COOKIE_NAME, CSRF_HEADER_NAME, HostBusinessError, HostBusinessFuture, HostBusinessRouter,
    HostConnectionContext, SESSION_COOKIE_NAME, SnapshotEnvelope, SnapshotRequest, WebHost,
    WebHostConfig, WebServerOwner, WebToken,
};
use serde_json::{Value, json};
use std::fs;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

const LOOPBACK: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
const TEST_TOKEN: &str = "e2e-fixed-test-token-20260921";

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn set_cookies(&self) -> String {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("set-cookie"))
            .filter_map(|(_, value)| value.split(';').next())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

async fn http_request(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> HttpResponse {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nContent-Length: {}\r\n",
        address.port(),
        body.len()
    );
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();

    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut bytes))
        .await
        .expect("HTTP server did not close the test connection")
        .unwrap();
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response has no header terminator");
    let (raw_headers, body) = bytes.split_at(split + 4);
    let mut lines = raw_headers.split(|byte| *byte == b'\n');
    let status = lines
        .next()
        .and_then(|line| {
            String::from_utf8_lossy(line)
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()
        })
        .expect("HTTP response has no status code");
    let headers = lines
        .filter_map(|line| {
            let line = String::from_utf8_lossy(line).trim().to_owned();
            let (name, value) = line.split_once(':')?;
            Some((name.to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect();
    HttpResponse {
        status,
        headers,
        body: body.to_vec(),
    }
}

fn allocate_port() -> u16 {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn cookie_value(cookies: &str, name: &str) -> String {
    cookies
        .split(';')
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then(|| value.to_owned())
        })
        .expect("login did not return the expected cookie")
}

fn json_body(response: &HttpResponse) -> Value {
    serde_json::from_slice(&response.body).expect("response body is not JSON")
}

fn host_config(static_root: &TempDir, upload_root: &TempDir, port: u16) -> WebHostConfig {
    WebHostConfig::new(
        LOOPBACK,
        port,
        WebToken::try_from(TEST_TOKEN.to_owned()).unwrap(),
        static_root.path().to_owned(),
        upload_root.path().to_owned(),
    )
    .unwrap()
}

#[derive(Debug, Default)]
struct RecordingRouter {
    dispatches: Mutex<Vec<Value>>,
    contexts: Mutex<Vec<HostConnectionContext>>,
    disconnects: AtomicUsize,
    snapshots: AtomicUsize,
}

impl HostBusinessRouter for RecordingRouter {
    fn dispatch<'a>(
        &'a self,
        context: HostConnectionContext,
        frame: keencode_acp::AcpIncomingFrame,
    ) -> HostBusinessFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let value = frame
                .into_json_rpc_value()
                .map_err(|_| HostBusinessError::InvalidAcp)?;
            self.dispatches
                .lock()
                .map_err(|_| HostBusinessError::Router)?
                .push(value.clone());
            self.contexts
                .lock()
                .map_err(|_| HostBusinessError::Router)?
                .push(context);
            let response = json!({
                "jsonrpc": "2.0",
                "id": value.get("id").cloned().unwrap_or_else(|| json!("e2e")),
                "result": if value.get("method").and_then(Value::as_str) == Some("initialize") {
                    json!({ "protocolVersion": 1 })
                } else {
                    json!({})
                }
            });
            serde_json::to_vec(&response)
                .map(Some)
                .map_err(|_| HostBusinessError::Router)
        })
    }

    fn disconnect(&self, _context: HostConnectionContext) {
        self.disconnects.fetch_add(1, Ordering::SeqCst);
    }

    fn snapshot<'a>(
        &'a self,
        _context: HostConnectionContext,
        _request: SnapshotRequest,
    ) -> HostBusinessFuture<'a, SnapshotEnvelope> {
        Box::pin(async move {
            self.snapshots.fetch_add(1, Ordering::SeqCst);
            Ok(SnapshotEnvelope {
                cursor: keencode_web::SnapshotCursor {
                    journal_sequence: 1,
                    next_delivery_sequence: 1,
                },
                payload: br#"{"type":"snapshot","recovered":true}"#.to_vec(),
            })
        })
    }
}

async fn login(address: SocketAddr) -> (String, String) {
    let response = http_request(
        address,
        "POST",
        "/api/auth/login",
        &[("Content-Type", "application/json".to_owned())],
        br#"{"token":"e2e-fixed-test-token-20260921"}"#,
    )
    .await;
    assert_eq!(response.status, 200);
    let cookies = response.set_cookies();
    let csrf = cookie_value(&cookies, CSRF_COOKIE_NAME);
    assert!(!cookie_value(&cookies, SESSION_COOKIE_NAME).is_empty());
    assert_eq!(json_body(&response)["tokenVersion"], 1);
    (cookies, csrf)
}

async fn wait_for_count(counter: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while counter.load(Ordering::SeqCst) < expected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("expected server lifecycle callback did not arrive");
}

async fn connect_business_ws(
    address: SocketAddr,
    cookies: &str,
    origin: &str,
) -> Result<
    (
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
        tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    let mut request = format!("ws://127.0.0.1:{}/api/ws", address.port())
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Origin", origin.parse().unwrap());
    request
        .headers_mut()
        .insert("Cookie", cookies.parse().unwrap());
    connect_async(request).await
}

async fn next_text(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
) -> String {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(value))) => return value.to_string(),
            Some(Ok(Message::Ping(value))) => {
                socket.send(Message::Pong(value)).await.unwrap();
            }
            Some(Ok(Message::Close(_))) | None => panic!("WebSocket closed before text frame"),
            Some(Ok(_)) => {}
            Some(Err(error)) => panic!("WebSocket read failed: {error}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_tcp_http_auth_csrf_upload_range_and_resource_isolation() {
    let static_root = tempfile::tempdir().unwrap();
    let upload_root = tempfile::tempdir().unwrap();
    fs::write(static_root.path().join("index.html"), b"ok").unwrap();
    let port = allocate_port();
    let host = Arc::new(WebHost::new(host_config(&static_root, &upload_root, port)).unwrap());
    let mut owner = WebServerOwner::start(Arc::clone(&host), host.router()).unwrap();
    let address = SocketAddr::new(LOOPBACK, port);

    let (cookies_one, csrf_one) = login(address).await;
    let (cookies_two, _) = login(address).await;
    let session_one = cookie_value(&cookies_one, SESSION_COOKIE_NAME);
    assert_ne!(session_one, cookie_value(&cookies_two, SESSION_COOKIE_NAME));

    let missing_csrf = http_request(
        address,
        "POST",
        "/api/uploads",
        &[
            ("Origin", format!("http://127.0.0.1:{port}")),
            ("Cookie", cookies_one.clone()),
            ("Content-Type", "application/octet-stream".to_owned()),
            ("X-KeenCode-File-Name", "photo.png".to_owned()),
        ],
        b"hello",
    )
    .await;
    assert_eq!(missing_csrf.status, 403);
    assert_eq!(json_body(&missing_csrf)["error"], "invalid_csrf");

    let uploaded = http_request(
        address,
        "POST",
        "/api/uploads",
        &[
            ("Origin", format!("http://127.0.0.1:{port}")),
            ("Cookie", cookies_one.clone()),
            (CSRF_HEADER_NAME, csrf_one.clone()),
            ("Content-Type", "text/plain".to_owned()),
            ("X-KeenCode-File-Name", "photo.png".to_owned()),
        ],
        b"hello",
    )
    .await;
    assert_eq!(uploaded.status, 200);
    let uploaded_json = json_body(&uploaded);
    assert_eq!(uploaded_json["fileName"], "photo.png");
    assert_eq!(uploaded_json["contentType"], "image/png");
    assert_eq!(uploaded_json["size"], 5);
    let resource_id = uploaded_json["resourceId"].as_str().unwrap();

    let ranged = http_request(
        address,
        "GET",
        &format!("/api/resources/{resource_id}"),
        &[
            ("Cookie", cookies_one.clone()),
            ("Range", "bytes=1-3".to_owned()),
        ],
        &[],
    )
    .await;
    assert_eq!(ranged.status, 206);
    assert_eq!(ranged.header("content-range"), Some("bytes 1-3/5"));
    assert_eq!(ranged.header("accept-ranges"), Some("bytes"));
    assert_eq!(ranged.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(ranged.body, b"ell");

    let cross_session = http_request(
        address,
        "GET",
        &format!("/api/resources/{resource_id}"),
        &[("Cookie", cookies_two)],
        &[],
    )
    .await;
    assert_eq!(cross_session.status, 404);
    assert_eq!(json_body(&cross_session)["error"], "resource_not_found");

    let no_session = http_request(
        address,
        "GET",
        &format!("/api/resources/{resource_id}"),
        &[],
        &[],
    )
    .await;
    assert_eq!(no_session.status, 401);

    owner.stop().await.unwrap();
    assert_eq!(host.status().active_connections, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_tcp_websocket_origin_auth_and_reconnect_rebinds_session() {
    let static_root = tempfile::tempdir().unwrap();
    let upload_root = tempfile::tempdir().unwrap();
    fs::write(static_root.path().join("index.html"), b"ok").unwrap();
    let port = allocate_port();
    let host = Arc::new(WebHost::new(host_config(&static_root, &upload_root, port)).unwrap());
    let router = Arc::new(RecordingRouter::default());
    let adapter =
        Arc::new(keencode_web::HostWsAdapter::from_router(Arc::clone(&router), 8).unwrap());
    let mut owner = WebServerOwner::start(
        Arc::clone(&host),
        host.router_with_business(Arc::clone(&adapter)),
    )
    .unwrap();
    let address = SocketAddr::new(LOOPBACK, port);
    let (cookies, _) = login(address).await;
    let origin = format!("http://127.0.0.1:{port}");

    let wrong_origin = connect_business_ws(address, &cookies, "http://localhost:1").await;
    assert!(matches!(
        wrong_origin,
        Err(tokio_tungstenite::tungstenite::Error::Http(response))
            if response.status().as_u16() == 403
    ));

    let (mut first, response) = connect_business_ws(address, &cookies, &origin)
        .await
        .expect("valid Origin should complete the WebSocket handshake");
    assert_eq!(response.status().as_u16(), 101);
    assert_eq!(
        next_text(&mut first).await,
        r#"{"type":"ready","transport":"websocket","protocol":"acp"}"#
    );
    first
        .send(Message::Text(
            r#"{"jsonrpc":"2.0","id":"init-1","method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#.into(),
        ))
        .await
        .unwrap();
    let initialize_response: Value = serde_json::from_str(&next_text(&mut first).await).unwrap();
    assert_eq!(initialize_response["id"], "init-1");
    assert_eq!(initialize_response["result"]["protocolVersion"], 1);

    first
        .send(Message::Text(
            r#"{"jsonrpc":"2.0","id":"load-1","method":"session/load","params":{"sessionId":"session-restore","cwd":"C:\\fixture","mcpServers":[]}}"#.into(),
        ))
        .await
        .unwrap();
    let load_response: Value = serde_json::from_str(&next_text(&mut first).await).unwrap();
    assert_eq!(load_response["id"], "load-1");
    assert_eq!(router.dispatches.lock().unwrap().len(), 2);

    first.close(None).await.unwrap();
    drop(first);
    wait_for_count(&router.disconnects, 1).await;

    let (mut second, response) = connect_business_ws(address, &cookies, &origin)
        .await
        .expect("the same authenticated session should reconnect");
    assert_eq!(response.status().as_u16(), 101);
    assert_eq!(
        next_text(&mut second).await,
        r#"{"type":"ready","transport":"websocket","protocol":"acp"}"#
    );
    second
        .send(Message::Text(
            r#"{"jsonrpc":"2.0","id":"load-2","method":"session/load","params":{"sessionId":"session-restore","cwd":"C:\\fixture","mcpServers":[]}}"#.into(),
        ))
        .await
        .unwrap();
    let second_load: Value = serde_json::from_str(&next_text(&mut second).await).unwrap();
    assert_eq!(second_load["id"], "load-2");
    assert_eq!(router.dispatches.lock().unwrap().len(), 3);

    let published = adapter
        .publish_for_session(
            "session-restore",
            Some(1),
            br#"{"type":"recovered-event"}"#.to_vec(),
        )
        .unwrap();
    assert_eq!(published, 1);
    assert_eq!(
        next_text(&mut second).await,
        r#"{"type":"recovered-event"}"#
    );

    second.close(None).await.unwrap();
    drop(second);
    wait_for_count(&router.disconnects, 2).await;
    owner.stop().await.unwrap();
    assert_eq!(
        router.snapshots.load(Ordering::SeqCst),
        0,
        "HTTP reconnect currently rebinds via session/load; no cursor query is exposed yet"
    );
}
