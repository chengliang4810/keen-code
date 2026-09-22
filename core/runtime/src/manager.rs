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
        let directory = keencode_resources::ensure_project_storage(
            &self.config.storage_root,
            &request.project_root,
        )?;
        keencode_resources::register_session_location(
            &self.config.storage_root,
            &session_id,
            &directory,
        )?;
        let mut config = self.config.clone();
        config.storage_root = directory;
        let session = RuntimeSession::create_session(config, request)?;
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
        let config = self.session_config(&session_id)?;
        recover_session_mutations(&config.storage_root, config.journal, config.artifacts)?;
        match RuntimeSession::open_session(config, session_id.as_str())? {
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

    /// 通过可重建索引返回全部 Session 元数据，不恢复未改变的历史正文。
    pub fn list_stored_sessions(&self) -> Result<Vec<StoredSessionMetadata>, RuntimeError> {
        self.list_stored_sessions_for_project(None)
    }

    /// 只读取指定项目的数据目录，不枚举其他项目的会话。
    pub fn list_stored_sessions_for_project(
        &self,
        project_root: Option<&str>,
    ) -> Result<Vec<StoredSessionMetadata>, RuntimeError> {
        let directories = if let Some(path) = project_root {
            keencode_resources::project_storage_for_path(&self.config.storage_root, path)?
                .into_iter()
                .collect()
        } else {
            keencode_resources::project_storage_directories(&self.config.storage_root)?
        };
        let mut listed = Vec::new();
        for directory in directories {
            recover_session_mutations(&directory, self.config.journal, self.config.artifacts)?;
            for session_id in list_session_ids(&directory)? {
                keencode_resources::register_session_location(
                    &self.config.storage_root,
                    &session_id,
                    &directory,
                )?;
                if let Some(metadata) =
                    SessionJournal::read_metadata(&directory, session_id, self.config.journal)?
                {
                    listed.push(metadata);
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

    /// 按精确 ID 读取权威元数据，不扫描其他 Session，也不信任列表缓存。
    pub fn stored_session_metadata(
        &self,
        session_id: &str,
    ) -> Result<StoredSessionMetadata, RuntimeError> {
        match self.get(session_id) {
            Ok(session) => {
                return session
                    .read_state(|state| StoredSessionMetadata::from_state(state, false))?
                    .ok_or(RuntimeError::SessionNotCreated);
            }
            Err(RuntimeError::SessionNotRegistered) => {}
            Err(error) => return Err(error),
        }
        let session_id = SessionId::new(session_id)?;
        let config = self.session_config(&session_id)?;
        match SessionJournal::open(&config.storage_root, session_id, config.journal)? {
            SessionOpen::Ready(journal) => journal
                .read_state(|state| StoredSessionMetadata::from_state(state, false))?
                .ok_or(RuntimeError::SessionNotCreated),
            SessionOpen::Corrupt(report) => {
                StoredSessionMetadata::from_state(&report.last_valid_state, true)
                    .ok_or(RuntimeError::SessionNotCreated)
            }
        }
    }

    fn session_config(&self, id: &SessionId) -> Result<RuntimeConfig, RuntimeError> {
        let directory =
            keencode_resources::session_project_directory(&self.config.storage_root, id)?
                .ok_or(RuntimeError::SessionNotCreated)?;
        if !directory
            .join(id.as_str())
            .try_exists()
            .map_err(|_| RuntimeError::SessionNotCreated)?
        {
            return Err(RuntimeError::SessionNotCreated);
        }
        let mut config = self.config.clone();
        config.storage_root = directory;
        Ok(config)
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
        match RuntimeSession::open_session(self.session_config(&session_id)?, session_id.as_str()) {
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
        let config = match self.session_config(&session_id) {
            Ok(config) => config,
            Err(RuntimeError::SessionNotCreated) => return Ok(false),
            Err(error) => return Err(error),
        };
        if !list_session_ids(&config.storage_root)?.contains(&session_id) {
            return Ok(false);
        }
        let lease = match SessionLease::try_acquire(&config.storage_root, session_id.clone())? {
            SessionLeaseAcquire::Acquired(lease) => lease,
            SessionLeaseAcquire::Busy { .. } => return Err(RuntimeError::SessionBusy),
        };
        drop(lease);
        let deleted = delete_session_storage(&config.storage_root, &session_id)?;
        keencode_resources::remove_session_location(&self.config.storage_root, &session_id)?;
        Ok(deleted)
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
        let config = self.session_config(&request.source_session_id)?;
        let result = fork_session(
            &config.storage_root,
            self.config.journal,
            self.config.artifacts,
            request,
        )?;
        keencode_resources::register_session_location(
            &self.config.storage_root,
            &result.session_id,
            &config.storage_root,
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
        let config = self.session_config(&request.source_session_id)?;
        let result = prepare_edit_user(
            &config.storage_root,
            self.config.journal,
            self.config.artifacts,
            request,
        )?;
        keencode_resources::register_session_location(
            &self.config.storage_root,
            &result.archived_session_id,
            &config.storage_root,
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
