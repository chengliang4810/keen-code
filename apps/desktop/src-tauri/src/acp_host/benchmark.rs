//! 评测进程使用的无窗口 ACP Host，只公开批量答题所需的标准方法。

use super::*;
use std::fs::File;
use std::io::Write;

/// 无窗口评测仍经严格 ACP JSON-RPC 边界进入 Runtime，不加载桌面设置或扩展。
pub(crate) struct BenchmarkAcpHost {
    runtime: Arc<AgentRuntime>,
    project_root: PathBuf,
    provider_id: String,
    model: String,
    decoder: AcpRequestDecoder,
    encoder: AcpResponseEncoder,
    initialized: Mutex<bool>,
    session_id: Mutex<Option<String>>,
    transcript: Mutex<File>,
}

impl BenchmarkAcpHost {
    pub(crate) fn new(
        runtime: Arc<AgentRuntime>,
        project_root: PathBuf,
        provider_id: String,
        model: String,
        transcript: File,
    ) -> Result<Self, String> {
        let project_root = std::fs::canonicalize(project_root)
            .map_err(|error| format!("评测项目目录无法规范化: {error}"))?;
        if !project_root.is_dir() || provider_id.is_empty() || model.is_empty() {
            return Err("评测 ACP Host 配置无效".to_owned());
        }
        let response_limits = AcpResponseLimits::new(ACP_RESPONSE_MAX_BYTES, 64, 65_536)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            runtime,
            project_root,
            provider_id,
            model,
            decoder: AcpRequestDecoder::new(),
            encoder: AcpResponseEncoder::with_limits(response_limits)
                .map_err(|error| error.to_string())?,
            initialized: Mutex::new(false),
            session_id: Mutex::new(None),
            transcript: Mutex::new(transcript),
        })
    }

    /// 记录原始请求和完整响应，确保评测结果可证明请求确实经过 ACP 边界。
    pub(crate) async fn dispatch(&self, message: Value) -> Result<Value, String> {
        self.record("request", &message)?;
        let request_id = request_id_from_value(&message).unwrap_or(schema::RequestId::Null);
        let response = match self.dispatch_inner(message).await {
            Ok(response) => response,
            Err(error) => {
                tracing::error!(%error, "Benchmark ACP request failed");
                serde_json::from_str(&error)
                    .unwrap_or(self.encoded_error_value(request_id, HostFailure::Internal)?)
            }
        };
        self.record("response", &response)?;
        Ok(response)
    }

    async fn dispatch_inner(&self, message: Value) -> Result<Value, String> {
        let request_id = request_id_from_value(&message).unwrap_or(schema::RequestId::Null);
        let raw = serde_json::to_vec(&message).map_err(|error| error.to_string())?;
        let incoming = self.decoder.decode_raw(&raw).map_err(|error| {
            self.encoded_error(request_id.clone(), boundary_failure(error))
                .unwrap_or_else(|_| "ACP 请求无法解码".to_owned())
        })?;
        let AcpIncomingFrame::Request(frame) = incoming else {
            return Err("评测 ACP Host 不接受通知".to_owned());
        };
        let (id, request) = frame.into_parts();
        if matches!(id, schema::RequestId::Null) {
            return Err(self.encoded_error(id, HostFailure::InvalidRequest)?);
        }
        if !matches!(&request, AcpRequest::Initialize(_)) && !self.is_initialized()? {
            return Err(self.encoded_error(id, HostFailure::AuthRequired)?);
        }
        let response = match request {
            AcpRequest::Initialize(request) => {
                let response = self.initialize(request)?;
                self.encode_result(id, &response)?
            }
            AcpRequest::NewSession(request) => {
                let response = self.new_session(request).await?;
                self.encode_result(id, &response)?
            }
            AcpRequest::SetSessionConfigOption(request) => {
                let response = self.set_config_option(request)?;
                self.encode_result(id, &response)?
            }
            AcpRequest::Prompt(request) => {
                let response = self.prompt(request).await?;
                self.encode_result(id, &response)?
            }
            _ => return Err(self.encoded_error(id, HostFailure::MethodNotFound)?),
        };
        Ok(response)
    }

    fn initialize(
        &self,
        request: schema::InitializeRequest,
    ) -> Result<keencode_acp::InitializeResponseDto, String> {
        if request.protocol_version != SUPPORTED_PROTOCOL_VERSION
            && request.protocol_version != schema::ProtocolVersion::LATEST
        {
            return Err("ACP initialize 协议版本无效".to_owned());
        }
        self.runtime
            .elicitation_coordinator()
            .negotiate_connection_capabilities(
                &keencode_acp::ConnectionId::new("benchmark-client")
                    .map_err(|_| "ACP benchmark 连接标识无效".to_owned())?,
                &request.client_capabilities,
            )
            .map_err(|_| "ACP Client 能力无效".to_owned())?;
        *self
            .initialized
            .lock()
            .map_err(|_| "ACP Host 状态锁失效".to_owned())? = true;
        let capabilities = keencode_acp::InitializeAgentCapabilitiesDto::new()
            .load_session(false)
            .prompt_capabilities(schema::PromptCapabilities::default())
            .mcp_capabilities(schema::McpCapabilities::default());
        let mut meta = Map::new();
        meta.insert(
            META_DEFAULT_CWD.to_owned(),
            Value::String(self.project_root.to_string_lossy().into_owned()),
        );
        Ok(
            keencode_acp::InitializeResponseDto::new(SUPPORTED_PROTOCOL_VERSION)
                .agent_capabilities(capabilities)
                .agent_info(Some(schema::Implementation::new("KeenCode Bench", "0.0.1")))
                .meta(Some(meta)),
        )
    }

    async fn new_session(
        &self,
        request: schema::NewSessionRequest,
    ) -> Result<schema::NewSessionResponse, String> {
        if !request.mcp_servers.is_empty() {
            return Err("评测 ACP Host 不加载 MCP".to_owned());
        }
        let requested = std::fs::canonicalize(&request.cwd)
            .map_err(|_| "ACP session/new cwd 无效".to_owned())?;
        if requested != self.project_root {
            return Err("ACP session/new cwd 不属于本次评测".to_owned());
        }
        let operation_id = operation_id(request.meta.as_ref())
            .map_err(|_| "ACP session/new operationId 无效".to_owned())?;
        let session = self
            .runtime
            .open_or_create_session(&self.project_root, None, &operation_id)
            .map_err(|error| error.to_string())?;
        let session_id = session.session_id().as_str().to_owned();
        self.runtime
            .focus_session(&session_id)
            .map_err(|error| error.to_string())?;
        self.runtime
            .ensure_session_delivery(&session_id)
            .map_err(|error| error.to_string())?;
        *self
            .session_id
            .lock()
            .map_err(|_| "ACP Host Session 锁失效".to_owned())? = Some(session_id.clone());
        let snapshot = session.snapshot().map_err(|error| error.to_string())?;
        Ok(
            schema::NewSessionResponse::new(schema::SessionId::new(session_id))
                .modes(session_mode_state(false))
                .config_options(self.config_options(&snapshot))
                .meta(Some(self.snapshot_meta(&snapshot, None))),
        )
    }

    fn set_config_option(
        &self,
        request: schema::SetSessionConfigOptionRequest,
    ) -> Result<schema::SetSessionConfigOptionResponse, String> {
        self.require_session(request.session_id.0.as_ref())?;
        let expected = format!("{}::{}", self.provider_id, self.model);
        if request.config_id.0.as_ref() != CONFIG_MODEL_ID || request.value.0.as_ref() != expected {
            return Err("评测 ACP Host 只接受本次评测模型".to_owned());
        }
        let operation_id = operation_id(request.meta.as_ref())
            .map_err(|_| "ACP model operationId 无效".to_owned())?;
        self.runtime
            .set_session_model(
                request.session_id.0.as_ref(),
                &operation_id,
                &self.provider_id,
                &self.model,
            )
            .map_err(|error| error.to_string())?;
        let snapshot = self
            .runtime
            .session_snapshot(request.session_id.0.as_ref())
            .map_err(|error| error.to_string())?;
        Ok(schema::SetSessionConfigOptionResponse::new(
            self.config_options(&snapshot),
        ))
    }

    async fn prompt(
        &self,
        request: schema::PromptRequest,
    ) -> Result<schema::PromptResponse, String> {
        let session_id = request.session_id.0.as_ref().to_owned();
        self.require_session(&session_id)?;
        let text = prompt_text(request.prompt).map_err(|_| "ACP Prompt 正文无效".to_owned())?;
        let turn_id = prompt_turn_id(request.meta.as_ref())
            .map_err(|_| "ACP Prompt turnId 无效".to_owned())?;
        if meta_bool(request.meta.as_ref(), META_ULTRA_MODE)
            .map_err(|_| "ACP Prompt ultraMode 无效".to_owned())?
        {
            return Err("评测 ACP Host 不启用 Ultra 模式".to_owned());
        }
        let session = self
            .runtime
            .open_or_create_session(
                &self.project_root,
                Some(&session_id),
                "benchmark-acp-prompt",
            )
            .map_err(|error| error.to_string())?;
        let mut subscription = session.subscribe().map_err(|error| error.to_string())?;
        self.runtime
            .start_root_turn(&session_id, &turn_id, &text, RootTurnOptions::default())
            .await
            .map_err(|error| error.to_string())?;
        let terminal = wait_for_terminal(&session, &turn_id, &mut subscription).await?;
        let stop_reason =
            prompt_stop_reason(&terminal).map_err(|_| "ACP Prompt 以执行错误结束".to_owned())?;
        let snapshot = session.snapshot().map_err(|error| error.to_string())?;
        Ok(schema::PromptResponse::new(stop_reason)
            .meta(Some(self.snapshot_meta(&snapshot, Some(&turn_id)))))
    }

    fn config_options(&self, snapshot: &RuntimeSnapshot) -> Vec<schema::SessionConfigOption> {
        let value = format!("{}::{}", self.provider_id, self.model);
        let current = snapshot
            .state
            .provider
            .as_ref()
            .map(|provider| format!("{}::{}", provider.provider_id, provider.model));
        vec![model_config_option(
            current,
            vec![schema::SessionConfigSelectOption::new(
                value,
                self.model.clone(),
            )],
        )]
    }

    fn snapshot_meta(&self, snapshot: &RuntimeSnapshot, turn_id: Option<&str>) -> schema::Meta {
        let mut meta = Map::new();
        let active_turn_id = active_root_turn(snapshot);
        meta.insert(
            META_SNAPSHOT.to_owned(),
            serde_json::json!({
                "sessionId": snapshot.state.session_id.as_str(),
                "state": if active_turn_id.is_some() { "streaming" } else { "ready" },
                "activeTurnId": active_turn_id,
                "backend": "acp",
                "projectPath": &snapshot.state.project_root,
                "title": &snapshot.state.title,
                "lastError": Value::Null,
                "diagnosticsPath": Value::Null,
            }),
        );
        if let Some(turn_id) = turn_id {
            meta.insert(META_TURN_ID.to_owned(), Value::String(turn_id.to_owned()));
        }
        meta
    }

    fn require_session(&self, session_id: &str) -> Result<(), String> {
        let selected = self
            .session_id
            .lock()
            .map_err(|_| "ACP Host Session 锁失效".to_owned())?;
        if selected.as_deref() == Some(session_id) {
            Ok(())
        } else {
            Err("ACP Session 不属于本次评测".to_owned())
        }
    }

    fn is_initialized(&self) -> Result<bool, String> {
        self.initialized
            .lock()
            .map(|state| *state)
            .map_err(|_| "ACP Host 状态锁失效".to_owned())
    }

    fn encode_result<T>(&self, id: schema::RequestId, result: &T) -> Result<Value, String>
    where
        T: AcpResponsePayload,
    {
        let raw = self
            .encoder
            .encode_result(id, result)
            .map_err(|error| error.to_string())?;
        serde_json::from_slice(&raw).map_err(|error| error.to_string())
    }

    fn encoded_error(&self, id: schema::RequestId, failure: HostFailure) -> Result<String, String> {
        let raw = self
            .encoder
            .encode_error(id, &failure.rpc_error())
            .map_err(|error| error.to_string())?;
        String::from_utf8(raw).map_err(|error| error.to_string())
    }

    fn encoded_error_value(
        &self,
        id: schema::RequestId,
        failure: HostFailure,
    ) -> Result<Value, String> {
        let raw = self
            .encoder
            .encode_error(id, &failure.rpc_error())
            .map_err(|error| error.to_string())?;
        serde_json::from_slice(&raw).map_err(|error| error.to_string())
    }

    fn record(&self, direction: &str, message: &Value) -> Result<(), String> {
        let mut transcript = self
            .transcript
            .lock()
            .map_err(|_| "ACP Transcript 锁失效".to_owned())?;
        serde_json::to_writer(
            &mut *transcript,
            &serde_json::json!({"direction": direction, "message": message}),
        )
        .map_err(|error| error.to_string())?;
        transcript
            .write_all(b"\n")
            .map_err(|error| error.to_string())
    }
}

async fn wait_for_terminal(
    session: &RuntimeSession,
    turn_id: &str,
    subscription: &mut RuntimeEventSubscription,
) -> Result<TerminalTurn, String> {
    loop {
        let snapshot = session.snapshot().map_err(|error| error.to_string())?;
        if let Some(turn) = snapshot.state.turns.values().find(|turn| {
            turn.turn_id.as_str() == turn_id
                && turn.source_agent_id.as_str() == ROOT_SOURCE_AGENT_ID
                && turn.parent_turn_id.is_none()
                && turn.status != TurnStatus::Running
        }) {
            return Ok(TerminalTurn {
                status: turn.status.clone(),
                stop_reason: turn.stop_reason,
            });
        }
        match subscription.recv().await {
            Ok(_) | Err(RuntimeEventReceiveError::Lagged(_)) => {}
            Err(RuntimeEventReceiveError::Closed) => {
                return Err("ACP Prompt 等待期间 Session 已关闭".to_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn formal_acp_boundary_records_initialize_and_new_session() {
        let fixture = tempfile::tempdir().unwrap();
        let project = fixture.path().join("project");
        let storage = fixture.path().join("storage");
        let transcript = fixture.path().join("acp-requests.jsonl");
        std::fs::create_dir(&project).unwrap();
        let runtime = AgentRuntime::new_for_control_test(&storage).unwrap();
        let host = BenchmarkAcpHost::new(
            Arc::clone(&runtime),
            project.clone(),
            "benchmark".to_owned(),
            "test-model".to_owned(),
            File::create(&transcript).unwrap(),
        )
        .unwrap();

        let initialize = host
            .dispatch(serde_json::json!({
                "jsonrpc": "2.0",
                "id": "initialize",
                "method": "initialize",
                "params": {"protocolVersion": 1, "clientCapabilities": {}}
            }))
            .await
            .unwrap();
        assert_eq!(initialize["result"]["protocolVersion"], 1);
        let created = host
            .dispatch(serde_json::json!({
                "jsonrpc": "2.0",
                "id": "new",
                "method": "session/new",
                "params": {
                    "cwd": project,
                    "mcpServers": [],
                    "_meta": {"keencode/operationId": "benchmark-new"}
                }
            }))
            .await
            .unwrap();
        assert!(
            created["result"]["sessionId"]
                .as_str()
                .unwrap()
                .starts_with("session-")
        );
        let unsupported = host
            .dispatch(serde_json::json!({
                "jsonrpc": "2.0",
                "id": "list",
                "method": "session/list",
                "params": {}
            }))
            .await
            .unwrap();
        assert_eq!(unsupported["error"]["code"], -32601);

        let records = std::fs::read_to_string(transcript).unwrap();
        assert_eq!(records.lines().count(), 6);
        assert!(records.contains("session/new"));
        assert!(records.contains("\"error\""));
        runtime.shutdown().await.unwrap();
    }
}
