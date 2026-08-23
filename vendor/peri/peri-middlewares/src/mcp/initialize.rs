use std::{collections::HashMap, path::Path, sync::Arc};

use super::{
    auth_store::FileCredentialStore,
    channel_handler::ChannelHandler,
    client::{
        build_http_transport, spawn_stdio_transport, ClientStatus, McpClientHandle, McpClientPool,
        McpInitStatus, McpServiceWrapper, OAuthStatus, HTTP_CONNECT_TIMEOUT, STDIO_CONNECT_TIMEOUT,
    },
    config::{ConfigSource, McpConfigError, McpConfigFile, OAuthConfig},
    oauth_flow::OAuthFlowEvent,
    transport::TransportConfig,
};

impl McpClientPool {
    pub async fn run_initialize(
        pool: Arc<Self>,
        cwd: &Path,
        claude_home: &Path,
        status_tx: tokio::sync::watch::Sender<McpInitStatus>,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        channel_handler: Option<Arc<ChannelHandler>>,
    ) {
        let (config, plugin_sources) = super::load_merged_config_full(cwd, claude_home);
        Self::run_initialize_config(
            pool,
            config,
            plugin_sources,
            status_tx,
            oauth_event_callback,
            channel_handler,
        )
        .await;
    }

    /// 只从宿主指定的单个配置文件初始化 MCP 连接池。
    ///
    /// 该入口不会读取 `~/.peri/settings.json`、`~/.claude` 或项目 `.mcp.json`，
    /// 适用于已经自行完成用户配置与插件配置合并的嵌入式宿主。
    pub async fn run_initialize_from_path(
        pool: Arc<Self>,
        config_path: &Path,
        status_tx: tokio::sync::watch::Sender<McpInitStatus>,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        channel_handler: Option<Arc<ChannelHandler>>,
    ) -> Result<(), McpConfigError> {
        let mut config = match super::config::load_from_path(config_path) {
            Ok(config) => config,
            Err(error) => {
                let message = error.to_string();
                let failed = McpInitStatus::Failed(message);
                let _ = status_tx.send(failed.clone());
                *pool.init_status.write() = failed;
                return Err(error);
            }
        };
        for server_config in config.mcp_servers.values_mut() {
            server_config.source = Some(ConfigSource::Project(config_path.to_path_buf()));
        }
        Self::run_initialize_from_config(
            pool,
            config,
            status_tx,
            oauth_event_callback,
            channel_handler,
        )
        .await;
        Ok(())
    }

    /// 使用调用方在进程内构造的配置初始化连接池。
    ///
    /// 嵌入式宿主可以在调用前从自己的安全存储解析插件配置，直接把结果
    /// 传入此入口；初始化过程不会要求调用方先把包含敏感值的配置写入文件。
    pub async fn run_initialize_from_config(
        pool: Arc<Self>,
        mut config: McpConfigFile,
        status_tx: tokio::sync::watch::Sender<McpInitStatus>,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        channel_handler: Option<Arc<ChannelHandler>>,
    ) {
        for server_config in config.mcp_servers.values_mut() {
            // 插件配置在宿主侧已经完成了包含安全存储值的完整插值；再次按
            // 进程环境展开会错误改写合法的 `${...}` 密钥内容。用户配置
            // 仍沿用 Peri 的环境变量展开规则。
            if !matches!(server_config.source, Some(ConfigSource::Plugin)) {
                *server_config = super::config::expand_server_config(server_config);
            }
        }
        Self::run_initialize_config(
            pool,
            config,
            HashMap::new(),
            status_tx,
            oauth_event_callback,
            channel_handler,
        )
        .await;
    }

    /// 使用调用方已经解析的配置执行统一连接流程。
    async fn run_initialize_config(
        pool: Arc<Self>,
        config: McpConfigFile,
        plugin_sources: HashMap<String, String>,
        status_tx: tokio::sync::watch::Sender<McpInitStatus>,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        channel_handler: Option<Arc<ChannelHandler>>,
    ) {
        let connectable = config
            .mcp_servers
            .iter()
            .filter(|(_, sc)| !sc.disabled.unwrap_or(false))
            .count();
        if config.mcp_servers.is_empty() {
            let _ = status_tx.send(McpInitStatus::Ready { total: 0 });
            *pool.init_status.write() = McpInitStatus::Ready { total: 0 };
            pool.mark_initialized();
            return;
        }

        *pool.plugin_sources.write() = plugin_sources;

        // OAuth 事件回调注入 pool（spawn_oauth_flow / start_oauth_flow 读取；
        // 无回调时授权不自动触发——由 host pool 统一执行，本 pool 仅标记
        // NeedsAuthorization，授权完成后经共享凭证文件恢复）。
        if let Some(cb) = oauth_event_callback {
            pool.set_oauth_event_callback(cb);
        }
        let token_store = Arc::new(FileCredentialStore::new());

        for (name, server_config) in &config.mcp_servers {
            pool.configs
                .write()
                .insert(name.clone(), server_config.clone());
        }
        let _ = status_tx.send(McpInitStatus::Initializing {
            connected: 0,
            total: connectable,
        });
        *pool.init_status.write() = McpInitStatus::Initializing {
            connected: 0,
            total: connectable,
        };

        let mut connected = 0usize;
        for (name, server_config) in &config.mcp_servers {
            // 跳过已禁用的服务器，注册为 Disabled 状态
            if server_config.disabled.unwrap_or(false) {
                tracing::info!(server = %name, "MCP 服务器已禁用，跳过连接");
                pool.clients.write().insert(
                    name.clone(),
                    Arc::new(McpClientHandle {
                        name: name.clone(),
                        peer: None,
                        tools: vec![],
                        resources: vec![],
                        status: ClientStatus::Disabled,
                        oauth_status: OAuthStatus::default(),
                        source: server_config.source.clone(),
                        url: server_config.url.clone(),
                        channel_capable: false,
                    }),
                );
                continue;
            }
            let transport_config = match TransportConfig::try_from(server_config) {
                Ok(tc) => tc,
                Err(e) => {
                    tracing::warn!(server = %name, error = %e, "传输层构建失败");
                    Self::insert_failed(&pool, name, format!("传输层构建失败: {e}"));
                    continue;
                }
            };
            let is_http = matches!(transport_config, TransportConfig::StreamableHttp { .. });
            let timeout = if is_http {
                HTTP_CONNECT_TIMEOUT
            } else {
                STDIO_CONNECT_TIMEOUT
            };

            let connect_result = match transport_config {
                TransportConfig::Stdio {
                    ref command,
                    ref args,
                    ref env,
                } => match spawn_stdio_transport(command, args, env) {
                    Ok(transport) => {
                        if let Some(ref handler) = channel_handler {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client(handler.clone(), transport),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Channel))
                        } else {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client((), transport),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Default))
                        }
                    }
                    Err(e) => {
                        Self::insert_failed(&pool, name, format!("stdio 启动失败: {e}"));
                        continue;
                    }
                },
                TransportConfig::StreamableHttp {
                    ref url,
                    ref headers,
                    ref oauth,
                } => {
                    let oauth_cfg = oauth.as_ref().cloned().or_else(|| {
                        // 无显式 OAuth 配置时：若凭证文件已有该 server 的 token，
                        // 用默认配置走恢复路径（run_oauth_flow 快速路径跳过浏览器）。
                        match tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(token_store.load_server(name))) {
                            Ok(Some(_)) => {
                                tracing::info!(server = %name, "发现已保存的 OAuth 凭证，使用默认配置恢复");
                                Some(OAuthConfig::default())
                            }
                            _ => None,
                        }
                    });
                    if oauth_cfg.is_some() {
                        if pool.oauth_event_callback().is_some() {
                            // host pool：不主动触发授权（避免启动即弹 popup
                            // 打扰），统一标记 NeedsAuthorization，由用户经
                            // MCP 面板显式发起（mcp/oauth_start RPC →
                            // spawn_oauth_flow → popup）。
                            Self::insert_needs_auth(&pool, name, "OAuth 授权待完成".to_string());
                            continue;
                        }
                        // TUI 面板池：无 UI 交互通道，走快速路径——尝试恢复
                        // 磁盘凭证直接连接（不弹窗）；凭据缺失/失效时保持
                        // NeedsAuthorization，由 host pool 授权后共享凭证文件
                        // 恢复。异步执行不阻塞初始化。
                        pool.spawn_oauth_flow(name);
                        continue;
                    } else {
                        if let Some(ref handler) = channel_handler {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client(
                                    handler.clone(),
                                    build_http_transport(url, headers),
                                ),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Channel))
                        } else {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client((), build_http_transport(url, headers)),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Default))
                        }
                    }
                }
            };

            match connect_result {
                Ok(Ok(rs)) => {
                    let tools = rs.list_all_tools().await.unwrap_or_default();
                    let resources = rs.list_all_resources().await.unwrap_or_default();
                    tracing::info!(server = %name, tools = tools.len(), resources = resources.len(), "MCP 连接成功");
                    let peer = rs.peer().clone();
                    let channel_capable = peer
                        .peer_info()
                        .and_then(|info| {
                            info.capabilities
                                .experimental
                                .as_ref()
                                .and_then(|exp| exp.get("claude/channel"))
                                .cloned()
                        })
                        .is_some();
                    let oauth_status = OAuthStatus::default();
                    let handle = Arc::new(McpClientHandle {
                        name: name.clone(),
                        peer: Some(peer),
                        tools,
                        resources,
                        status: ClientStatus::Connected,
                        oauth_status,
                        source: server_config.source.clone(),
                        url: server_config.url.clone(),
                        channel_capable,
                    });
                    pool.clients.write().insert(name.clone(), handle);
                    pool.services.lock().await.insert(name.clone(), rs);
                    connected += 1;
                    let _ = status_tx.send(McpInitStatus::Initializing {
                        connected,
                        total: connectable,
                    });
                    *pool.init_status.write() = McpInitStatus::Initializing {
                        connected,
                        total: connectable,
                    };
                }
                Ok(Err(e)) => {
                    let err_str = e.to_string();
                    tracing::warn!(server = %name, error = %err_str, "MCP 连接失败");
                    if Self::is_auth_required_error(&err_str, is_http) {
                        // 服务器要求授权（如 sentry 401）：标记待授权，不主动
                        // 触发——用户经 MCP 面板显式发起授权（mcp/oauth_start）。
                        Self::insert_needs_auth(&pool, name, err_str);
                    } else {
                        Self::insert_failed(&pool, name, err_str);
                    }
                }
                Err(_) => {
                    Self::insert_failed(&pool, name, "连接超时".to_string());
                }
            }
        }

        if connectable > 0 && connected == 0 {
            let all_need_auth = pool
                .clients
                .read()
                .values()
                .all(|h| h.oauth_status == OAuthStatus::NeedsAuthorization);
            if all_need_auth {
                let _ = status_tx.send(McpInitStatus::Ready { total: 0 });
                *pool.init_status.write() = McpInitStatus::Ready { total: 0 };
            } else {
                let failed: Vec<String> = pool
                    .clients
                    .read()
                    .iter()
                    .filter(|(_, h)| matches!(h.status, ClientStatus::Failed(_)))
                    .map(|(n, h)| {
                        if let ClientStatus::Failed(r) = &h.status {
                            format!("{}: {}", n, r)
                        } else {
                            n.clone()
                        }
                    })
                    .collect();
                let _ = status_tx.send(McpInitStatus::Failed(format!(
                    "{} 个服务器连接失败: {}",
                    connectable,
                    failed.join("; ")
                )));
                *pool.init_status.write() = McpInitStatus::Failed(format!(
                    "{} 个服务器连接失败: {}",
                    connectable,
                    failed.join("; ")
                ));
            }
        } else {
            let _ = status_tx.send(McpInitStatus::Ready { total: connected });
            *pool.init_status.write() = McpInitStatus::Ready { total: connected };
        }
        // 初始化收口：此后状态变化才产生上下线通知（初始连接结果由
        // 会话首 turn 的 first_turn_reminder 概览覆盖，不逐条推送）。
        pool.mark_initialized();
    }

    pub async fn initialize(
        cwd: &Path,
        claude_home: &Path,
        oauth_event_callback: Option<Box<dyn Fn(OAuthFlowEvent) + Send + Sync>>,
        channel_handler: Option<Arc<ChannelHandler>>,
    ) -> Self {
        let (config, plugin_sources) = super::load_merged_config_full(cwd, claude_home);
        let pool = Arc::new(Self::new_pending());
        *pool.plugin_sources.write() = plugin_sources;
        let token_store = Arc::new(FileCredentialStore::new());
        // OAuth 事件回调注入 pool（spawn_oauth_flow / start_oauth_flow 读取；
        // 无回调时授权不自动触发，仅标记 NeedsAuthorization）。
        if let Some(cb) = oauth_event_callback {
            pool.set_oauth_event_callback(cb);
        }

        for (name, sc) in &config.mcp_servers {
            pool.configs.write().insert(name.clone(), sc.clone());
        }

        for (name, server_config) in &config.mcp_servers {
            // 跳过已禁用的服务器，注册为 Disabled 状态
            if server_config.disabled.unwrap_or(false) {
                tracing::info!(server = %name, "MCP 服务器已禁用，跳过连接");
                pool.clients.write().insert(
                    name.clone(),
                    Arc::new(McpClientHandle {
                        name: name.clone(),
                        peer: None,
                        tools: vec![],
                        resources: vec![],
                        status: ClientStatus::Disabled,
                        oauth_status: OAuthStatus::default(),
                        source: server_config.source.clone(),
                        url: server_config.url.clone(),
                        channel_capable: false,
                    }),
                );
                continue;
            }
            let tc = match TransportConfig::try_from(server_config) {
                Ok(tc) => tc,
                Err(e) => {
                    Self::insert_failed(&pool, name, format!("传输层构建失败: {e}"));
                    continue;
                }
            };
            let is_http = matches!(tc, TransportConfig::StreamableHttp { .. });
            let timeout = if is_http {
                HTTP_CONNECT_TIMEOUT
            } else {
                STDIO_CONNECT_TIMEOUT
            };

            let connect_result = match tc {
                TransportConfig::Stdio {
                    ref command,
                    ref args,
                    ref env,
                } => match spawn_stdio_transport(command, args, env) {
                    Ok(t) => {
                        if let Some(ref handler) = channel_handler {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client(handler.clone(), t),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Channel))
                        } else {
                            tokio::time::timeout(timeout, rmcp::service::serve_client((), t))
                                .await
                                .map(|inner| inner.map(McpServiceWrapper::Default))
                        }
                    }
                    Err(e) => {
                        Self::insert_failed(&pool, name, format!("stdio 失败: {e}"));
                        continue;
                    }
                },
                TransportConfig::StreamableHttp {
                    ref url,
                    ref headers,
                    ref oauth,
                } => {
                    let oauth_cfg = oauth.as_ref().cloned().or_else(|| {
                        // 无显式 OAuth 配置时：若凭证文件已有该 server 的 token，
                        // 用默认配置走恢复路径（run_oauth_flow 快速路径跳过浏览器）。
                        match tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(token_store.load_server(name))) {
                            Ok(Some(_)) => {
                                tracing::info!(server = %name, "发现已保存的 OAuth 凭证，使用默认配置恢复");
                                Some(OAuthConfig::default())
                            }
                            _ => None,
                        }
                    });
                    if oauth_cfg.is_some() {
                        if pool.oauth_event_callback().is_some() {
                            // host pool：不主动触发授权（避免启动即弹 popup
                            // 打扰），统一标记 NeedsAuthorization，由用户经
                            // MCP 面板显式发起（mcp/oauth_start RPC →
                            // spawn_oauth_flow → popup）。
                            Self::insert_needs_auth(&pool, name, "OAuth 授权待完成".to_string());
                            continue;
                        }
                        // TUI 面板池：无 UI 交互通道，走快速路径——尝试恢复
                        // 磁盘凭证直接连接（不弹窗）；凭据缺失/失效时保持
                        // NeedsAuthorization，由 host pool 授权后共享凭证文件
                        // 恢复。异步执行不阻塞初始化。
                        pool.spawn_oauth_flow(name);
                        continue;
                    } else {
                        if let Some(ref handler) = channel_handler {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client(
                                    handler.clone(),
                                    build_http_transport(url, headers),
                                ),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Channel))
                        } else {
                            tokio::time::timeout(
                                timeout,
                                rmcp::service::serve_client((), build_http_transport(url, headers)),
                            )
                            .await
                            .map(|inner| inner.map(McpServiceWrapper::Default))
                        }
                    }
                }
            };

            match connect_result {
                Ok(Ok(rs)) => {
                    let tools = rs.list_all_tools().await.unwrap_or_default();
                    let resources = rs.list_all_resources().await.unwrap_or_default();
                    let peer = rs.peer().clone();
                    let channel_capable = peer
                        .peer_info()
                        .and_then(|info| {
                            info.capabilities
                                .experimental
                                .as_ref()
                                .and_then(|exp| exp.get("claude/channel"))
                                .cloned()
                        })
                        .is_some();
                    let oauth_status = OAuthStatus::default();
                    pool.clients.write().insert(
                        name.clone(),
                        Arc::new(McpClientHandle {
                            name: name.clone(),
                            peer: Some(peer),
                            tools,
                            resources,
                            status: ClientStatus::Connected,
                            oauth_status,
                            source: server_config.source.clone(),
                            url: server_config.url.clone(),
                            channel_capable,
                        }),
                    );
                    pool.services.lock().await.insert(name.clone(), rs);
                }
                Ok(Err(e)) => {
                    let err_str = e.to_string();
                    if Self::is_auth_required_error(&err_str, is_http) {
                        // 服务器要求授权（如 sentry 401）：标记待授权，不主动
                        // 触发——用户经 MCP 面板显式发起授权（mcp/oauth_start）。
                        Self::insert_needs_auth(&pool, name, err_str);
                    } else {
                        Self::insert_failed(&pool, name, err_str);
                    }
                }
                Err(_) => {
                    Self::insert_failed(&pool, name, "连接超时".into());
                }
            }
        }

        Arc::try_unwrap(pool).unwrap_or_else(|arc| {
            let p = arc.as_ref();
            let cloned = Self::new_pending();
            *cloned.clients.write() = p.clients.read().clone();
            *cloned.configs.write() = p.configs.read().clone();
            *cloned.plugin_sources.write() = p.plugin_sources.read().clone();
            *cloned.init_status.write() = p.init_status.read().clone();
            cloned.initialized.store(
                p.initialized.load(std::sync::atomic::Ordering::SeqCst),
                std::sync::atomic::Ordering::SeqCst,
            );
            cloned
        })
    }
}
