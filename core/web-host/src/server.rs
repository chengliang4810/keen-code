//! 真正覆盖 TCP/HTTP/WebSocket 生命周期的 Web Server owner。
//!
//! Axum 的请求并发层只限制单个 request 在途时间，WebSocket upgrade 后不会继续
//! 占用该 permit。本模块在 accepted IO 上持有 semaphore permit，直到 HTTP/WS
//! 连接对象被 hyper 释放，因而上限覆盖完整连接生命周期。

use crate::{WebError, WebHost};
use axum::Router;
use axum::serve::IncomingStream;
use axum::serve::Listener;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio::task::JoinHandle;

/// 默认 graceful shutdown 等待时长；超时后 owner 会中止 server task。
pub const DEFAULT_GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// 持有 TCP 连接的 semaphore permit；连接关闭时同步减少 WebHost active 数。
struct ConnectionPermit {
    _permit: OwnedSemaphorePermit,
    host: Arc<WebHost>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.host.connection_closed();
    }
}

/// 把连接 permit 绑定到 hyper 使用的 IO，覆盖 HTTP keep-alive 和 WebSocket upgrade。
struct LimitedIo {
    stream: TcpStream,
    _connection: ConnectionPermit,
}

impl AsyncRead for LimitedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(context, buffer)
    }
}

impl AsyncWrite for LimitedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

/// Axum Listener 适配器；达到连接上限时暂停 accept，保留 OS backlog 而不创建任务。
struct LimitedListener {
    listener: TcpListener,
    permits: Arc<Semaphore>,
    host: Arc<WebHost>,
}

impl LimitedListener {
    fn new(listener: TcpListener, host: Arc<WebHost>) -> Self {
        let permits = Arc::new(Semaphore::new(host.status().max_connections));
        Self {
            listener,
            permits,
            host,
        }
    }
}

impl Listener for LimitedListener {
    type Io = LimitedIo;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .expect("Web Server owner semaphore must remain open");
        loop {
            match self.listener.accept().await {
                Ok((stream, address)) => {
                    self.host.connection_opened();
                    return (
                        LimitedIo {
                            stream,
                            _connection: ConnectionPermit {
                                _permit: permit,
                                host: Arc::clone(&self.host),
                            },
                        },
                        address,
                    );
                }
                Err(error) if is_retryable_accept_error(&error) => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(error) => {
                    // Axum Listener 的契约要求 accept 自己处理错误；继续等待避免
                    // 单次系统错误退出整个 Host。固定连接 permit 在本轮循环中保留。
                    let _ = error;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

/// ConnectInfo extension 的本地包装：`SocketAddr` 与 `IncomingStream` 都是
/// 外部类型，直接实现 `Connected` 会触发孤儿规则，故用 newtype 承载对端地址。
/// 登录失败限流按对端地址分桶依赖这个 extension。
#[derive(Clone, Copy, Debug)]
pub struct PeerConnectInfo(pub SocketAddr);

impl axum::extract::connect_info::Connected<IncomingStream<'_, LimitedListener>>
    for PeerConnectInfo
{
    fn connect_info(stream: IncomingStream<'_, LimitedListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

fn is_retryable_accept_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted
    )
}

/// 已启动 Web Server 的生命周期 owner。
pub struct WebServerOwner {
    host: Arc<WebHost>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    shutdown_timeout: Duration,
}

impl fmt::Debug for WebServerOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebServerOwner")
            .field("status", &self.status())
            .field("shutdown_timeout", &self.shutdown_timeout)
            .finish()
    }
}

impl WebServerOwner {
    /// 绑定已配置的固定地址并启动 Axum server task。
    pub fn start(host: Arc<WebHost>, router: Router) -> Result<Self, WebError> {
        Self::start_with_timeout(host, router, DEFAULT_GRACEFUL_SHUTDOWN_TIMEOUT)
    }

    /// 启动 server，并显式配置 graceful shutdown 等待预算。
    pub fn start_with_timeout(
        host: Arc<WebHost>,
        router: Router,
        shutdown_timeout: Duration,
    ) -> Result<Self, WebError> {
        if shutdown_timeout.is_zero() {
            return Err(WebError::InvalidConfig(
                "Web Server shutdown 超时必须大于 0".to_owned(),
            ));
        }
        let listener = host.bind_listener()?;
        let listener = LimitedListener::new(listener, Arc::clone(&host));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task_host = Arc::clone(&host);
        let task = tokio::spawn(async move {
            let shutdown = async move {
                let _ = shutdown_rx.await;
            };
            let result = axum::serve(
                listener,
                // 必须注入 ConnectInfo：登录失败限流按对端地址分桶，缺少该
                // extension 时所有请求都会退化为同一个 "unknown-peer" 全局限流。
                router.into_make_service_with_connect_info::<PeerConnectInfo>(),
            )
            .with_graceful_shutdown(shutdown)
            .await;
            if result.is_err() {
                task_host.server_failed();
            }
        });
        Ok(Self {
            host,
            shutdown: Some(shutdown_tx),
            task: Some(task),
            shutdown_timeout,
        })
    }

    /// 返回 transport 与认证状态的安全快照。
    pub fn status(&self) -> crate::WebHostStatus {
        self.host.status()
    }

    /// 等待 server task 自然结束；不会主动发送 shutdown。
    pub async fn join(&mut self) -> Result<(), WebError> {
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        match task.await {
            Ok(()) => {
                self.host.server_stopped();
                Ok(())
            }
            Err(error) if error.is_cancelled() => {
                self.host.server_stopped();
                Ok(())
            }
            Err(error) => {
                self.host.server_failed();
                Err(WebError::Internal(format!("Web Server task 失败：{error}")))
            }
        }
    }

    /// 停止接受新连接，等待现有 HTTP/WS 连接收敛，超时后中止 task。
    pub async fn stop(&mut self) -> Result<(), WebError> {
        if self.task.is_none() {
            self.host.server_stopped();
            return Ok(());
        }
        self.host.server_stopping();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(task) = self.task.as_mut() else {
            return Ok(());
        };
        if tokio::time::timeout(self.shutdown_timeout, &mut *task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
            self.host.server_failed();
            return Err(WebError::Internal(
                "Web Server graceful shutdown 超时".to_owned(),
            ));
        }
        self.task.take();
        self.host.server_stopped();
        Ok(())
    }
}

impl Drop for WebServerOwner {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WebHostConfig, WebHostState, WebToken};
    use std::fs;
    use std::net::IpAddr;

    #[tokio::test]
    async fn owner_starts_and_gracefully_stops_server_task() {
        let static_root = tempfile::tempdir().unwrap();
        let upload_root = tempfile::tempdir().unwrap();
        fs::write(static_root.path().join("index.html"), b"ok").unwrap();
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let config = WebHostConfig::new(
            IpAddr::from([127, 0, 0, 1]),
            port,
            WebToken::try_from("test-token-123456789".to_owned()).unwrap(),
            static_root.path().to_owned(),
            upload_root.path().to_owned(),
        )
        .unwrap();
        let host = Arc::new(WebHost::new(config).unwrap());
        let mut owner = WebServerOwner::start(Arc::clone(&host), host.router()).unwrap();
        assert_eq!(owner.status().state, WebHostState::Running);
        owner.stop().await.unwrap();
        assert_eq!(owner.status().state, WebHostState::Stopped);
        assert_eq!(owner.status().active_connections, 0);
    }
}
