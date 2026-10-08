use super::{HttpResponseReadError, read_http_response_limited};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

/// 启动单次本地 HTTP 服务并返回其阻塞式响应。
fn fixture_response(raw_response: &'static str) -> reqwest::blocking::Response {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("绑定 HTTP 测试端口");
    let address = listener.local_addr().expect("读取 HTTP 测试端口");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("接受 HTTP 测试请求");
        // Windows 会在关闭仍有未读取请求数据的 socket 时发送 RST；先消费完整
        // 请求头，确保客户端稳定收到受控响应。
        let mut reader = BufReader::new(stream.try_clone().expect("复制 HTTP 测试连接"));
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).expect("读取 HTTP 测试请求");
            if read == 0 || line == "\r\n" || line == "\n" {
                break;
            }
        }
        stream
            .write_all(raw_response.as_bytes())
            .expect("写入 HTTP 测试响应");
    });
    let response = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .expect("构建 HTTP 测试客户端")
        .get(format!("http://{address}/fixture"))
        .send()
        .expect("请求 HTTP 测试服务");
    server.join().expect("HTTP 测试服务线程不应 panic");
    response
}

/// Content-Length 已声明超限时必须在读取截断正文前返回 TooLarge。
#[test]
fn content_length_over_limit_is_rejected_before_body_read() {
    let response =
        fixture_response("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n");

    let error = read_http_response_limited(response, 4).unwrap_err();

    assert!(matches!(
        error,
        HttpResponseReadError::TooLarge { max_bytes: 4 }
    ));
}

/// Chunked 响应没有 Content-Length 时仍必须按实际解码正文限制大小。
#[test]
fn chunked_response_without_length_is_stream_limited() {
    let response = fixture_response(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n",
    );
    assert_eq!(response.content_length(), None);

    let error = read_http_response_limited(response, 4).unwrap_err();

    assert!(matches!(
        error,
        HttpResponseReadError::TooLarge { max_bytes: 4 }
    ));
}

/// 正文恰好等于上限时必须完整返回，不能产生边界误判。
#[test]
fn body_exactly_at_limit_is_accepted() {
    let response =
        fixture_response("HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcd");

    let bytes = read_http_response_limited(response, 4).expect("恰好达到上限应成功");

    assert_eq!(bytes, b"abcd");
}

/// 响应在声明长度前提前关闭时必须保留独立的 Read 错误类型。
#[test]
fn truncated_body_is_reported_as_read_error() {
    let response =
        fixture_response("HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nab");

    let error = read_http_response_limited(response, 4).unwrap_err();

    assert!(matches!(error, HttpResponseReadError::Read(_)));
}
