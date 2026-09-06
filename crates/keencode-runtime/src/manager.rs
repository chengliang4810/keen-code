//! 进程内 Session 注册、隔离查找、关闭与精确取消控制面。

use std::collections::BTreeMap;
use std::sync::Mutex;

use keencode_resources::{
    SessionEditUserRequest, SessionEditUserResult, SessionForkRequest, SessionForkResult,
    SessionId, SessionJournal, SessionLease, SessionLeaseAcquire, SessionMessage, SessionOpen,
    delete_session_storage, fork_session, list_session_ids, prepare_edit_user,
    recover_session_mutations,
};

use crate::{
    ActiveRuntimeTurn, CreateSessionRequest, OpenSessionResult, RuntimeConfig, RuntimeError,
    RuntimeSession, StoredSessionMetadata, TurnCancellationOutcome,
};

/// 进程内唯一管理多个相互隔离 Runtime Session 的注册表。
pub struct RuntimeManager {
    /// 所有新建和打开 Session 共同使用的不可变本地资源配置。
    config: RuntimeConfig,
    /// 按稳定 SessionId 注册且在 create/open/close 间原子检查的 Session 集合。
    sessions: Mutex<BTreeMap<SessionId, RuntimeSession>>,
}

impl RuntimeManager {
    /// 校验配置并创建尚未注册任何 Session 的 RuntimeManager。
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        config.validate()?;
        recover_session_mutations(&config.storage_root, config.journal, config.artifacts)?;
        Ok(Self {
            config,
            sessions: Mutex::new(BTreeMap::new()),
        })
    }

    /// 创建并登记全新 Session，进程内相同 SessionId 的并发注册只允许一个成功。
    pub fn create(&self, request: CreateSessionRequest) -> Result<RuntimeSession, RuntimeError> {
        let session_id = SessionId::new(request.session_id.clone())?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if sessions.contains_key(&session_id) {
            return Err(RuntimeError::SessionAlreadyRegistered);
        }
        let session = RuntimeSession::create_session(self.config.clone(), request)?;
        sessions.insert(session_id, session.clone());
        Ok(session)
    }

    /// 打开并登记现有 Session；损坏报告不会进入可操作 Session 注册表。
    pub fn open(&self, session_id: impl Into<String>) -> Result<OpenSessionResult, RuntimeError> {
        let session_id = SessionId::new(session_id.into())?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if sessions.contains_key(&session_id) {
            return Err(RuntimeError::SessionAlreadyRegistered);
        }
        match RuntimeSession::open_session(self.config.clone(), session_id.as_str())? {
            OpenSessionResult::Ready(session) => {
                sessions.insert(session_id, session.clone());
                Ok(OpenSessionResult::Ready(session))
            }
            OpenSessionResult::Corrupt(report) => Ok(OpenSessionResult::Corrupt(report)),
        }
    }

    /// 返回已注册 Session 的受控共享句柄，不会打开磁盘上的未注册 Session。
    pub fn get(&self, session_id: impl Into<String>) -> Result<RuntimeSession, RuntimeError> {
        let session_id = SessionId::new(session_id.into())?;
        self.sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?
            .get(&session_id)
            .cloned()
            .ok_or(RuntimeError::SessionNotRegistered)
    }

    /// 返回磁盘中全部 Session 的无正文元数据，并优先读取当前已注册句柄的最新状态。
    pub fn list_stored_sessions(&self) -> Result<Vec<StoredSessionMetadata>, RuntimeError> {
        let registered = self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?
            .clone();
        let mut listed = Vec::new();
        for session_id in list_session_ids(&self.config.storage_root)? {
            if let Some(session) = registered.get(&session_id) {
                listed.push(StoredSessionMetadata::from_state(
                    &session.snapshot()?.state,
                    false,
                )?);
                continue;
            }
            match SessionJournal::open(&self.config.storage_root, session_id, self.config.journal)?
            {
                SessionOpen::Ready(journal) => {
                    listed.push(StoredSessionMetadata::from_state(&journal.state()?, false)?);
                }
                SessionOpen::Corrupt(report) => {
                    listed.push(StoredSessionMetadata::from_state(
                        &report.last_valid_state,
                        true,
                    )?);
                }
            }
        }
        listed.sort_by(|left, right| {
            right
                .updated_at_unix_ms
                .cmp(&left.updated_at_unix_ms)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        Ok(listed)
    }

    /// 返回当前进程已经打开且尚未被 Manager 关闭的全部 Session 标识。
    pub fn registered_session_ids(&self) -> Result<Vec<SessionId>, RuntimeError> {
        Ok(self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?
            .keys()
            .cloned()
            .collect())
    }

    /// 读取指定健康 Session 的完整 Transcript，不把只读历史永久登记到运行集合。
    pub fn session_transcript(
        &self,
        session_id: impl Into<String>,
    ) -> Result<Vec<SessionMessage>, RuntimeError> {
        let session_id = SessionId::new(session_id.into())?;
        if let Some(session) = self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?
            .get(&session_id)
            .cloned()
        {
            return session.transcript();
        }
        match RuntimeSession::open_session(self.config.clone(), session_id.as_str()) {
            Ok(OpenSessionResult::Ready(session)) => session.transcript(),
            Ok(OpenSessionResult::Corrupt(_)) => Err(RuntimeError::SessionCorrupt),
            Err(RuntimeError::SessionBusy) => match self.get(session_id.as_str()) {
                Ok(session) => session.transcript(),
                Err(RuntimeError::SessionNotRegistered) => Err(RuntimeError::SessionBusy),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        }
    }

    /// 返回当前进程全部尚未形成可确认终态的 Turn。
    pub fn active_turns(&self) -> Result<Vec<ActiveRuntimeTurn>, RuntimeError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?
            .clone();
        let mut turns = Vec::new();
        for (session_id, session) in sessions {
            turns.extend(
                session
                    .active_turn_ids()?
                    .into_iter()
                    .map(|turn_id| ActiveRuntimeTurn {
                        session_id: session_id.clone(),
                        turn_id,
                    }),
            );
        }
        turns.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        Ok(turns)
    }

    /// 返回仍有 Turn、子 Agent、工具、终端或工作树需要收尾的 Session 标识。
    pub fn active_session_ids(&self) -> Result<Vec<SessionId>, RuntimeError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?
            .clone();
        let mut active = Vec::new();
        for (session_id, session) in sessions {
            if session.has_active_work()? {
                active.push(session_id);
            }
        }
        active.sort();
        Ok(active)
    }

    /// 从进程内注册表关闭一个 Session；已发出的共享句柄仍按 Rust 所有权自然存活。
    pub fn close(&self, session_id: impl Into<String>) -> Result<(), RuntimeError> {
        let session_id = SessionId::new(session_id.into())?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        let session = sessions
            .get(&session_id)
            .ok_or(RuntimeError::SessionNotRegistered)?;
        session.close_runtime()?;
        sessions.remove(&session_id);
        Ok(())
    }

    /// 触发全部已注册 Session 的 Turn 取消并关闭 Manager 持有的所有句柄。
    ///
    /// 某个 Session 关闭失败不会阻止其他 Session 收敛；已确认关闭的项会立即从
    /// Manager 移除，未确认项则保留以便调用方重试对账。
    pub fn close_all(&self) -> Result<(), RuntimeError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        let session_ids = sessions.keys().cloned().collect::<Vec<_>>();
        let mut first_error = None;
        for session_id in session_ids {
            let result = sessions
                .get(&session_id)
                .ok_or(RuntimeError::SessionNotRegistered)
                .and_then(RuntimeSession::close_runtime);
            match result {
                Ok(()) => {
                    sessions.remove(&session_id);
                }
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// 永久删除一个未被当前 Manager 或其他进程打开的 Session 目录。
    pub fn delete(&self, session_id: impl Into<String>) -> Result<bool, RuntimeError> {
        let session_id = SessionId::new(session_id.into())?;
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if sessions.contains_key(&session_id) {
            return Err(RuntimeError::SessionOpenForDeletion);
        }
        if !list_session_ids(&self.config.storage_root)?.contains(&session_id) {
            return Ok(false);
        }
        let lease = match SessionLease::try_acquire(&self.config.storage_root, session_id.clone())?
        {
            SessionLeaseAcquire::Acquired(lease) => lease,
            SessionLeaseAcquire::Busy { .. } => return Err(RuntimeError::SessionBusy),
        };
        drop(lease);
        delete_session_storage(&self.config.storage_root, &session_id).map_err(RuntimeError::from)
    }

    /// 对已经从当前注册表关闭的源 Session 执行可恢复完整分支事务。
    pub fn fork_closed_session(
        &self,
        request: SessionForkRequest,
    ) -> Result<SessionForkResult, RuntimeError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if sessions.contains_key(&request.source_session_id) {
            return Err(RuntimeError::SessionBusy);
        }
        let result = fork_session(
            &self.config.storage_root,
            self.config.journal,
            self.config.artifacts,
            request,
        )?;
        Ok(result)
    }

    /// 对已经从当前注册表关闭的源 Session 原子归档并截断指定根用户 Turn。
    pub fn prepare_edit_user_closed_session(
        &self,
        request: SessionEditUserRequest,
    ) -> Result<SessionEditUserResult, RuntimeError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if sessions.contains_key(&request.source_session_id) {
            return Err(RuntimeError::SessionBusy);
        }
        let result = prepare_edit_user(
            &self.config.storage_root,
            self.config.journal,
            self.config.artifacts,
            request,
        )?;
        Ok(result)
    }

    /// 仅在指定 Session 的指定 Turn 正在执行时触发 Runtime 权威取消令牌。
    pub fn cancel_turn(
        &self,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Result<TurnCancellationOutcome, RuntimeError> {
        let session = self.get(session_id)?;
        session.cancel_turn(turn_id)
    }
}
