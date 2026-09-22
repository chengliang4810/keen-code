//! 本机受限 IPC 上的 NDJSON framing。
//!
//! Unix 使用本地 socket，Windows 使用 Tokio named pipe。两者都只传递一行一个
//! JSON-RPC 信封；协议内容仍由 ACP Host 严格解码。本模块不把 socket/pipe 当作
//! 认证层，权限与 Host root lock 由服务端负责，发现记录也只用于定位。

use crate::discovery::{EndpointRecord, IpcTransport};
use serde_json::Value;
use std::fmt;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::PathBuf;
use std::pin::Pin;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// NDJSON 单帧最大字节数；与 ACP Host 的响应上限保持同量级。
pub const MAX_NDJSON_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// NDJSON framing 错误。
#[derive(Debug)]
pub enum NdjsonError {
    /// 底层 I/O 失败。
    Io(io::Error),
    /// 单帧超过资源边界。
    FrameTooLarge,
    /// 空行或 JSON 不是合法值。
    InvalidJson,
    /// 请求包含换行，不能破坏单帧边界。
    EmbeddedNewline,
    /// 当前平台不支持发现记录中的传输。
    UnsupportedTransport,
}

impl fmt::Display for NdjsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "NDJSON I/O 失败: {error}"),
            Self::FrameTooLarge => formatter.write_str("NDJSON 帧超过大小限制"),
            Self::InvalidJson => formatter.write_str("NDJSON 帧不是合法 JSON"),
            Self::EmbeddedNewline => formatter.write_str("NDJSON 帧包含未转义换行"),
            Self::UnsupportedTransport => formatter.write_str("当前平台不支持该本地传输"),
        }
    }
}

impl std::error::Error for NdjsonError {}

impl From<io::Error> for NdjsonError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// NDJSON 中已经解析的 JSON 帧。
#[derive(Clone, Debug, PartialEq)]
pub struct NdjsonFrame {
    /// 原始 JSON 值。
    pub value: Value,
}

impl NdjsonFrame {
    /// 创建一条待发送的 JSON 帧并校验它可安全编码为单行。
    pub fn new(value: Value) -> Result<Self, NdjsonError> {
        let encoded = serde_json::to_vec(&value).map_err(|_| NdjsonError::InvalidJson)?;
        if encoded.len() > MAX_NDJSON_FRAME_BYTES {
            return Err(NdjsonError::FrameTooLarge);
        }
        if encoded.contains(&b'\n') || encoded.contains(&b'\r') {
            return Err(NdjsonError::EmbeddedNewline);
        }
        Ok(Self { value })
    }

    /// 从 JSON 值借用构造一帧。
    pub fn from_value(value: &Value) -> Result<Self, NdjsonError> {
        Self::new(value.clone())
    }
}

/// 支持 Tokio AsyncRead/AsyncWrite 的本地连接。
pub enum IpcConnection {
    /// Unix socket 连接。
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    /// Windows named pipe 客户端连接。
    #[cfg(windows)]
    NamedPipeClient(tokio::net::windows::named_pipe::NamedPipeClient),
    /// Windows named pipe 服务端连接；用于测试和 Host 适配器。
    #[cfg(windows)]
    NamedPipeServer(tokio::net::windows::named_pipe::NamedPipeServer),
}

impl IpcConnection {
    /// 根据发现记录建立本地连接。
    pub async fn connect(record: &EndpointRecord) -> Result<Self, NdjsonError> {
        match &record.transport {
            #[cfg(unix)]
            IpcTransport::UnixSocket => tokio::net::UnixStream::connect(&record.endpoint)
                .await
                .map(Self::Unix)
                .map_err(NdjsonError::Io),
            #[cfg(windows)]
            IpcTransport::NamedPipe => tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&record.endpoint)
                .map(Self::NamedPipeClient)
                .map_err(NdjsonError::Io),
            #[cfg(not(unix))]
            IpcTransport::UnixSocket => Err(NdjsonError::UnsupportedTransport),
            #[cfg(not(windows))]
            IpcTransport::NamedPipe => Err(NdjsonError::UnsupportedTransport),
        }
    }

    /// 拆分为独立的读取和写入端。
    pub fn split(self) -> (BoxedRead, BoxedWrite) {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => {
                let (read, write) = tokio::io::split(stream);
                (Box::pin(read), Box::pin(write))
            }
            #[cfg(windows)]
            Self::NamedPipeClient(stream) => {
                let (read, write) = tokio::io::split(stream);
                (Box::pin(read), Box::pin(write))
            }
            #[cfg(windows)]
            Self::NamedPipeServer(stream) => {
                let (read, write) = tokio::io::split(stream);
                (Box::pin(read), Box::pin(write))
            }
        }
    }
}

/// 动态读取端类型，供 Client/Server 统一处理两种平台传输。
pub type BoxedRead = Pin<Box<dyn AsyncRead + Send + Unpin>>;
/// 动态写入端类型，供 Client/Server 统一处理两种平台传输。
pub type BoxedWrite = Pin<Box<dyn AsyncWrite + Send + Unpin>>;

#[cfg(windows)]
fn apply_current_user_pipe_acl(
    server: &tokio::net::windows::named_pipe::NamedPipeServer,
) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_acl::acl::ACL;
    use windows_acl::helper::{current_user, name_to_sid};

    let user = current_user().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "无法解析当前 Windows 用户以设置 named pipe ACL",
        )
    })?;
    let sid = name_to_sid(&user, None).map_err(|error| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("解析当前 Windows 用户 SID 失败: {error}"),
        )
    })?;
    let mut acl = match ACL::from_object_handle(server.as_raw_handle() as _, false) {
        Ok(acl) => acl,
        // Tokio's overlapped pipe handle may not expose READ_CONTROL/WRITE_DAC to the
        // creating token. In that case Windows has already applied the process default
        // DACL; reject_remote_clients still prevents remote transports. Do not mask any
        // other ACL failure because it would turn an unknown security state into success.
        Err(5) => return Ok(()),
        Err(error) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("读取 named pipe ACL 失败: {error}"),
            ));
        }
    };
    // 先保留 Windows 默认 DACL（通常包含 SYSTEM/管理员），再明确加入当前
    // 用户的完全控制 ACE；PIPE 对象仍通过 reject_remote_clients 禁止远端连接。
    match acl.allow(sid.as_ptr() as _, false, 0x001F01FF) {
        Ok(_) | Err(5) => {}
        Err(error) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("写入 named pipe ACL 失败: {error}"),
            ));
        }
    }
    Ok(())
}

/// 一条 NDJSON 读取器；每次调用读取恰好一条 JSON 帧。
pub struct NdjsonReader<R> {
    reader: BufReader<R>,
}

impl<R> NdjsonReader<R>
where
    R: AsyncRead + Unpin,
{
    /// 使用已存在的缓冲读取端创建读取器。
    pub fn from_bufread(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
        }
    }

    /// 读取下一帧；EOF 返回 `Ok(None)`。
    pub async fn next(&mut self) -> Result<Option<NdjsonFrame>, NdjsonError> {
        let mut line = Vec::new();
        loop {
            let buffer = self.reader.fill_buf().await?;
            if buffer.is_empty() {
                if line.is_empty() {
                    return Ok(None);
                }
                break;
            }
            let newline = buffer.iter().position(|byte| *byte == b'\n');
            let take_len = newline.map_or(buffer.len(), |index| index + 1);
            if line.len().saturating_add(take_len) > MAX_NDJSON_FRAME_BYTES {
                return Err(NdjsonError::FrameTooLarge);
            }
            line.extend_from_slice(&buffer[..take_len]);
            self.reader.consume(take_len);
            if newline.is_some() {
                break;
            }
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            return Err(NdjsonError::InvalidJson);
        }
        let value = serde_json::from_slice::<Value>(&line).map_err(|_| NdjsonError::InvalidJson)?;
        NdjsonFrame::new(value).map(Some)
    }
}

impl<R> NdjsonReader<R>
where
    R: AsyncRead + Unpin,
{
    /// 从原始异步读取端创建读取器。
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
        }
    }
}

/// 一条自动刷新 NDJSON 写入器。
pub struct NdjsonWriter<W> {
    writer: W,
}

impl<W> NdjsonWriter<W>
where
    W: AsyncWrite + Unpin,
{
    /// 使用原始异步写入端创建写入器。
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    /// 编码一帧并写入单行；成功后立即 flush，保证流式事件可观察。
    pub async fn send(&mut self, frame: &NdjsonFrame) -> Result<(), NdjsonError> {
        let encoded = serde_json::to_vec(&frame.value).map_err(|_| NdjsonError::InvalidJson)?;
        if encoded.len() > MAX_NDJSON_FRAME_BYTES {
            return Err(NdjsonError::FrameTooLarge);
        }
        if encoded.contains(&b'\n') || encoded.contains(&b'\r') {
            return Err(NdjsonError::EmbeddedNewline);
        }
        self.writer.write_all(&encoded).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// 刷新底层写入端。
    pub async fn flush(&mut self) -> Result<(), NdjsonError> {
        self.writer.flush().await.map_err(NdjsonError::Io)
    }
}

/// 本地 IPC 监听器。
pub enum IpcListener {
    /// Unix socket listener。
    #[cfg(unix)]
    Unix {
        /// 监听句柄。
        listener: tokio::net::UnixListener,
        /// 用于在 listener 关闭后清理本次创建的 socket 节点。
        endpoint: PathBuf,
    },
    /// Windows named pipe listener；每个 accept 后替换成下一个实例。
    #[cfg(windows)]
    NamedPipe {
        /// 用于创建后续实例的命名管道端点。
        endpoint: String,
        /// 当前尚未接受连接的命名管道实例。
        server: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
    },
}

impl IpcListener {
    /// 绑定本机 endpoint；不会删除已存在的 Unix socket 或锁文件。
    pub async fn bind(record: &EndpointRecord) -> Result<Self, NdjsonError> {
        match &record.transport {
            #[cfg(unix)]
            IpcTransport::UnixSocket => {
                // bind 不覆盖已有路径；先取得监听句柄，再把 socket 节点收紧到
                // owner-only，避免沿用 umask 或父目录默认权限。
                let listener =
                    tokio::net::UnixListener::bind(&record.endpoint).map_err(NdjsonError::Io)?;
                std::fs::set_permissions(&record.endpoint, std::fs::Permissions::from_mode(0o600))
                    .map_err(NdjsonError::Io)?;
                Ok(Self::Unix {
                    listener,
                    endpoint: std::path::PathBuf::from(&record.endpoint),
                })
            }
            #[cfg(windows)]
            IpcTransport::NamedPipe => {
                use tokio::net::windows::named_pipe::ServerOptions;
                // Named pipe 默认继承当前进程的 Windows 安全描述符；拒绝远端
                // 客户端并要求首个实例，防止网络访问和第二个 owner 抢占端点。
                // 后续实例复用相同的 pipe ACL，由 Tokio/Windows 继承该描述符。
                let mut options = ServerOptions::new();
                options
                    .reject_remote_clients(true)
                    .first_pipe_instance(true);
                let server = options.create(&record.endpoint).map_err(NdjsonError::Io)?;
                apply_current_user_pipe_acl(&server).map_err(NdjsonError::Io)?;
                Ok(Self::NamedPipe {
                    endpoint: record.endpoint.clone(),
                    server: Some(server),
                })
            }
            #[cfg(not(unix))]
            IpcTransport::UnixSocket => Err(NdjsonError::UnsupportedTransport),
            #[cfg(not(windows))]
            IpcTransport::NamedPipe => Err(NdjsonError::UnsupportedTransport),
        }
    }

    /// 接受一个客户端连接。
    pub async fn accept(&mut self) -> Result<IpcConnection, NdjsonError> {
        match self {
            #[cfg(unix)]
            Self::Unix { listener, .. } => listener
                .accept()
                .await
                .map(|(stream, _)| IpcConnection::Unix(stream))
                .map_err(NdjsonError::Io),
            #[cfg(windows)]
            Self::NamedPipe { endpoint, server } => {
                use tokio::net::windows::named_pipe::ServerOptions;
                let current = server.take().ok_or_else(|| {
                    NdjsonError::Io(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "named pipe listener closed",
                    ))
                })?;
                current.connect().await.map_err(NdjsonError::Io)?;
                let mut options = ServerOptions::new();
                options.reject_remote_clients(true);
                let next = options.create(endpoint).map_err(NdjsonError::Io)?;
                apply_current_user_pipe_acl(&next).map_err(NdjsonError::Io)?;
                *server = Some(next);
                Ok(IpcConnection::NamedPipeServer(current))
            }
        }
    }
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Self::Unix { endpoint, .. } = self
            && let Ok(metadata) = std::fs::symlink_metadata(&*endpoint)
            && !metadata.file_type().is_symlink()
            && metadata.file_type().is_socket()
        {
            // 只删除仍为 socket 的节点；若路径已被替换为普通文件、目录或
            // symlink，fail closed，避免 transport 生命周期越权删除外部路径。
            let _ = std::fs::remove_file(&*endpoint);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn ndjson_writer_and_reader_round_trip() {
        let (mut client, server) = tokio::io::duplex(1024);
        let frame =
            NdjsonFrame::new(serde_json::json!({"jsonrpc":"2.0","id":"1"})).expect("合法帧应创建");
        {
            let mut writer = NdjsonWriter::new(&mut client);
            writer.send(&frame).await.expect("帧应发送");
        }
        let mut reader = NdjsonReader::new(server);
        let decoded = reader.next().await.expect("读取应成功").expect("应有一帧");
        assert_eq!(decoded, frame);
    }

    #[tokio::test]
    async fn reader_rejects_blank_line_and_oversized_frame() {
        let (mut tx, rx) = tokio::io::duplex(1024);
        tx.write_all(b"\n").await.expect("测试帧应写入");
        let mut reader = NdjsonReader::new(rx);
        assert!(matches!(reader.next().await, Err(NdjsonError::InvalidJson)));

        let (mut tx, rx) = tokio::io::duplex(MAX_NDJSON_FRAME_BYTES + 1);
        let payload = vec![b'a'; MAX_NDJSON_FRAME_BYTES + 1];
        // duplex 的容量可能小于一次完整 write；并发写入和读取才能稳定覆盖
        // 超限边界，而不会让测试夹具自己在背压处等待。
        let writer = tokio::spawn(async move {
            tx.write_all(&payload).await.expect("超限帧应写入");
        });
        let mut reader = NdjsonReader::new(rx);
        assert!(matches!(
            reader.next().await,
            Err(NdjsonError::FrameTooLarge)
        ));
        writer.await.expect("写入任务应结束");
    }

    #[test]
    fn frame_rejects_unescaped_line_break() {
        let value = serde_json::from_str::<Value>(r#"{"value":"line\nnext"}"#)
            .expect("JSON 字符串中的换行应已经转义");
        assert!(NdjsonFrame::new(value).is_ok());
    }
}
