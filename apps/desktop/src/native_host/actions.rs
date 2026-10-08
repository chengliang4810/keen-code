//! 原生 UI 动作直接进入 Rust 领域边界，回执仅在真实提交后返回。

use super::{NativeHost, projection, queue::QueuedInputSubmission, ui_error, ui_error_with_code};
use crate::{agent_runtime::RootTurnOptions, native_ui::model::*};
use base64::Engine as _;
use keencode_resources::{self as resource, SessionId, TurnId};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, path::Path};

impl NativeHost {
    pub(super) fn workspace_page(
        &self,
        root_path: &str,
        cursor: Option<PageCursor>,
        include_archived: bool,
    ) -> Result<WorkspacePage, NativeUiError> {
        let mut records = self.inner.runtime.stored_sessions().map_err(ui_error)?;
        // 侧栏只读一次小型水位台账，不逐个打开历史 Session 或反复解析文件。
        let watermarks = self.inner.attention.read_watermarks().map_err(ui_error)?;
        let subscribed = self
            .inner
            .native_session_subscribers
            .lock()
            .expect("原生订阅锁已损坏")
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let is_unread = |metadata: &resource::StoredSessionMetadata| {
            !subscribed.contains(metadata.session_id.as_str())
                && metadata.last_sequence > 0
                && watermarks
                    .get(metadata.session_id.as_str())
                    .is_none_or(|sequence| metadata.last_sequence > *sequence)
        };
        let projects = crate::workspace::project_records(&self.inner.paths).map_err(ui_error)?;
        if !root_path.is_empty() {
            crate::session_commands::authorize_stored_root(&self.inner.paths, root_path)
                .map_err(ui_error)?;
        }
        records.retain(|record| {
            (include_archived || !record.archived)
                // 当前项目决定新会话根目录，侧栏仍可访问其他已登记项目的历史。
                && projects.iter().any(|project| same_path(&record.project_root, &project.path))
        });
        records.sort_by_key(|record| {
            (
                std::cmp::Reverse(record.pinned),
                std::cmp::Reverse(record.updated_at_unix_ms),
            )
        });
        let mut groups = Vec::new();
        let mut group_revisions = std::collections::HashMap::new();
        for project in &projects {
            let snapshot = self.inner.sidebar.load(&project.path).map_err(ui_error)?;
            group_revisions.insert(project.path.clone(), snapshot.revision);
            groups.extend(snapshot.groups.into_iter().map(|group| GroupFact {
                group_id: group.group_id,
                project_key: group.root_path.clone(),
                root_path: group.root_path,
                title: group.title,
                session_ids: group.session_ids,
                revision: snapshot.revision,
            }));
        }
        let facts = projects
            .into_iter()
            .map(|project| ProjectFact {
                project_key: project.path.clone(),
                display_name: project.name,
                root_path: project.path.clone(),
                group_revision: group_revisions
                    .get(&project.path)
                    .copied()
                    .unwrap_or_default(),
                session_count: records
                    .iter()
                    .filter(|record| same_path(&record.project_root, &project.path))
                    .count(),
                unread_count: records
                    .iter()
                    .filter(|record| {
                        same_path(&record.project_root, &project.path) && is_unread(record)
                    })
                    .count(),
                archived: false,
                pinned: false,
            })
            .collect();
        let offset = cursor
            .as_ref()
            .and_then(|cursor| cursor.after_sequence)
            .unwrap_or(0) as usize;
        let sessions = records
            .iter()
            .skip(offset)
            .take(DEFAULT_PAGE_SIZE)
            .map(|metadata| SessionFact {
                session_id: metadata.session_id.as_str().to_owned(),
                project_key: crate::path_utils::path_text_to_frontend(&metadata.project_root),
                title: metadata.title.clone(),
                status: if metadata.corrupt {
                    SessionStatus::Corrupt
                } else {
                    match metadata.status {
                        resource::SessionStatus::Running => SessionStatus::Running,
                        resource::SessionStatus::Waiting => SessionStatus::Waiting,
                        _ => SessionStatus::Idle,
                    }
                },
                pinned: metadata.pinned,
                archived: metadata.archived,
                unread: is_unread(metadata),
                model: None,
                vision_enabled: false,
                effort: None,
                permission: PermissionMode::Build,
                plan: PlanMode::Off,
                followup_mode: FollowupMode::Queue,
                usage: None,
                prompt_history: PromptHistoryFact::default(),
                updated_at_unix_ms: metadata.updated_at_unix_ms,
                last_sequence: metadata.last_sequence,
            })
            .collect();
        Ok(WorkspacePage {
            projects: facts,
            sessions,
            groups,
            next: (offset + DEFAULT_PAGE_SIZE < records.len()).then_some(JournalCursor {
                after_sequence: Some((offset + DEFAULT_PAGE_SIZE) as u64),
                before_sequence: None,
                through_sequence: 0,
            }),
        })
    }

    pub(super) fn group_sessions(
        &self,
        project_key: &str,
        group_id: &str,
        session_ids: &[String],
        expected_revision: u64,
        operation_id: &str,
    ) -> Result<(), NativeUiError> {
        let project = self.inner.sidebar.load(project_key).map_err(ui_error)?;
        let records = self.inner.runtime.stored_sessions().map_err(ui_error)?;
        for id in session_ids {
            if !records.iter().any(|record| {
                record.session_id.as_str() == id
                    && same_path(&record.project_root, &project.root_path)
            }) {
                return Err(ui_error("分组只能包含当前项目的已有会话"));
            }
        }
        self.inner
            .sidebar
            .group_sessions(
                project_key,
                group_id,
                session_ids,
                expected_revision,
                operation_id,
            )
            .map_err(ui_error)?;
        Ok(())
    }

    fn project_id_for_root(&self, project_root: &str) -> Result<String, NativeUiError> {
        crate::workspace::project_records(&self.inner.paths)
            .map_err(ui_error)?
            .into_iter()
            .find(|project| same_path(&project.path, project_root))
            .map(|project| project.id)
            .ok_or_else(|| ui_error("项目尚未登记，无法修改项目设置"))
    }

    pub(super) async fn execute_action(
        &self,
        action: NativeUiAction,
    ) -> Result<NativeActionReceipt, NativeUiError> {
        let mut operation_id = operation_id(&action);
        let mut result_session = action_session(&action);
        match action {
            NativeUiAction::OpenProject { project_root } => {
                self.workspace_page(&project_root, None, false)?;
                let root = crate::session_commands::authorize_stored_root(
                    &self.inner.paths,
                    &project_root,
                )
                .map_err(ui_error)?;
                self.ensure_extension_candidate(&root)
                    .await
                    .map_err(ui_error)?;
            }
            NativeUiAction::LoadWorkspace {
                root_path,
                cursor,
                include_archived,
            } => {
                self.workspace_page(&root_path, cursor, include_archived)?;
            }
            NativeUiAction::CreateProject {
                path,
                name,
                create_workspace_root_if_missing,
                ..
            } => {
                let path = path.filter(|path| !path.trim().is_empty());
                crate::workspace::project_create(
                    &self.inner.paths,
                    path,
                    name,
                    create_workspace_root_if_missing,
                )
                .map_err(ui_error)?;
                result_session = None;
            }
            NativeUiAction::RenameProject {
                project_root, name, ..
            } => {
                let project_id = self.project_id_for_root(&project_root)?;
                crate::workspace::project_rename(&self.inner.paths, &project_id, &name)
                    .map_err(ui_error)?;
                result_session = None;
            }
            NativeUiAction::ForgetProject { project_root, .. } => {
                let project_id = self.project_id_for_root(&project_root)?;
                crate::workspace::project_remove(&self.inner.paths, &project_id)
                    .map_err(ui_error)?;
                result_session = None;
            }
            NativeUiAction::ReorderProjects { project_roots, .. } => {
                let project_ids = project_roots
                    .iter()
                    .map(|project_root| self.project_id_for_root(project_root))
                    .collect::<Result<Vec<_>, _>>()?;
                crate::workspace::projects_reorder(&self.inner.paths, &project_ids)
                    .map_err(ui_error)?;
                result_session = None;
            }
            NativeUiAction::CreateSession {
                project_root,
                operation_id,
            } => {
                let root = crate::session_commands::authorize_stored_root(
                    &self.inner.paths,
                    &project_root,
                )
                .map_err(ui_error)?;
                self.ensure_extension_candidate(&root)
                    .await
                    .map_err(ui_error)?;
                let session = self
                    .inner
                    .runtime
                    .open_or_create_session_serialized(&root, None, &operation_id)
                    .await
                    .map_err(ui_error)?;
                result_session = Some(session.session_id().as_str().to_owned());
                self.inner
                    .runtime
                    .focus_session(result_session.as_deref().unwrap())
                    .map_err(ui_error)?;
            }
            NativeUiAction::OpenSession {
                session_id,
                project_root,
            } => {
                let root = crate::session_commands::authorize_stored_root(
                    &self.inner.paths,
                    &project_root,
                )
                .map_err(ui_error)?;
                self.ensure_extension_candidate(&root)
                    .await
                    .map_err(ui_error)?;
                let session = self.open_session(&session_id)?;
                let actual = session
                    .read_state(|state| state.project_root.clone())
                    .map_err(ui_error)?;
                if !same_path(&actual, &project_root) {
                    return Err(ui_error("会话与选择的项目不一致"));
                }
                self.inner
                    .runtime
                    .focus_session(&session_id)
                    .map_err(ui_error)?;
            }
            NativeUiAction::RenameSession {
                session_id,
                title,
                operation_id,
            } => {
                self.open_session(&session_id)?
                    .rename(&operation_id, title, Some(resource::TitleSource::Manual))
                    .map_err(ui_error)?;
            }
            NativeUiAction::DeleteSession { session_id, .. } => {
                let context = crate::session_commands::close_session_for_mutation(
                    &self.inner.runtime,
                    &self.inner.paths,
                    &session_id,
                )
                .await
                .map_err(ui_error)?;
                crate::session_commands::retry_session_mutation(|| {
                    self.inner
                        .runtime
                        .runtime_manager()
                        .delete(session_id.clone())
                })
                .await
                .map_err(ui_error)?;
                drop(context);
                self.inner.drafts.clear(&session_id).map_err(ui_error)?;
            }
            NativeUiAction::SetSessionArchived {
                session_id,
                archived,
                operation_id,
            } => {
                self.open_session(&session_id)?
                    .set_preference(&operation_id, None, Some(archived))
                    .map_err(ui_error)?;
            }
            NativeUiAction::SetSessionPinned {
                session_id,
                pinned,
                operation_id,
            } => {
                self.open_session(&session_id)?
                    .set_preference(&operation_id, Some(pinned), None)
                    .map_err(ui_error)?;
            }
            NativeUiAction::GroupSessions {
                project_key,
                group_id,
                session_ids,
                expected_revision,
                operation_id,
            } => {
                self.group_sessions(
                    &project_key,
                    &group_id,
                    &session_ids,
                    expected_revision,
                    &operation_id,
                )?;
                result_session = None;
            }
            NativeUiAction::SetDraft {
                session_id,
                draft,
                edit_generation,
            } => {
                if draft.text.len() > 1024 * 1024 || draft.attachments.len() > 32 {
                    return Err(ui_error("草稿超过大小限制"));
                }
                self.open_session(&session_id)?;
                // 发送清理后仍可能收到同一编辑代次的 TextInput 回调；它不是
                // 新用户事实，不能重新写回已发送正文。
                if self.accept_draft_edit(&session_id, edit_generation) {
                    // 文本编辑与附件增删是不同的操作。延迟的输入回执不能携带旧附件
                    // 列表覆盖已确认的 Add/RemoveAttachment 结果。
                    let mut confirmed = self.draft(&session_id)?;
                    confirmed.text = draft.text;
                    confirmed.mention_query = draft.mention_query;
                    self.inner
                        .drafts
                        .set(&session_id, &confirmed)
                        .map_err(ui_error)?;
                    let _ = self
                        .inner
                        .local_changes
                        .send((session_id, Some(NativeUiEvent::DraftChanged(confirmed))));
                }
            }
            NativeUiAction::Send {
                session_id,
                text,
                attachments,
                model,
                effort,
                permission,
                plan,
                draft_edit_generation,
                operation_id,
            } => {
                let root = crate::session_commands::authorize_stored_session_root(
                    &self.inner.runtime,
                    &self.inner.paths,
                    &session_id,
                )
                .map_err(ui_error)?;
                self.ensure_extension_candidate(&root)
                    .await
                    .map_err(ui_error)?;
                let session = self.open_session(&session_id)?;
                if session.has_active_work().map_err(ui_error)? {
                    self.enqueue_input(
                        &session_id,
                        &operation_id,
                        QueuedInputSubmission {
                            text,
                            attachments,
                            model,
                            effort,
                            permission,
                            plan,
                        },
                    )?;
                } else {
                    self.select_turn_options(
                        &session_id,
                        &operation_id,
                        model.as_ref(),
                        effort.as_deref(),
                        permission,
                        plan,
                    )?;
                    let options = self.turn_options(&session_id, &attachments, plan)?;
                    self.inner
                        .runtime
                        .start_root_turn(
                            &session_id,
                            &native_root_turn_id(&operation_id),
                            &text,
                            options,
                        )
                        .await
                        .map_err(ui_error)?;
                }
                self.clear_draft_after_send(&session_id, draft_edit_generation)?;
                self.ensure_queue_supervisor(&session_id)?;
            }
            NativeUiAction::SetModel {
                session_id,
                selection,
                operation_id,
            } => {
                self.open_session(&session_id)?;
                self.inner
                    .runtime
                    .set_session_model(
                        &session_id,
                        &operation_id,
                        &selection.provider_id,
                        &selection.model,
                    )
                    .map_err(ui_error)?;
            }
            NativeUiAction::SetEffort {
                session_id,
                effort,
                operation_id,
            } => {
                self.open_session(&session_id)?;
                self.inner
                    .runtime
                    .set_session_effort(&session_id, &operation_id, &effort)
                    .map_err(ui_error)?;
            }
            NativeUiAction::SetVision {
                session_id,
                enabled,
                operation_id,
            } => {
                self.open_session(&session_id)?;
                self.inner
                    .runtime
                    .set_vision_enabled_with_operation(&session_id, &operation_id, enabled)
                    .map_err(ui_error)?;
            }
            NativeUiAction::SetPermission {
                session_id,
                mode,
                operation_id,
            } => {
                self.open_session(&session_id)?;
                self.persist_permission(&session_id, &operation_id, mode)?;
            }
            NativeUiAction::SetPlan {
                session_id,
                mode,
                operation_id,
            } => {
                let session = self.open_session(&session_id)?;
                session
                    .set_plan(
                        &operation_id,
                        resource::PlanState {
                            enabled: mode == PlanMode::ReadOnly,
                            ..Default::default()
                        },
                    )
                    .map_err(ui_error)?;
            }
            NativeUiAction::SetFollowupMode {
                session_id,
                mode,
                operation_id,
            } => {
                self.open_session(&session_id)?;
                self.inner
                    .runtime
                    .set_session_followup_mode(
                        &session_id,
                        &operation_id,
                        match mode {
                            FollowupMode::Queue => resource::FollowupMode::Queue,
                            FollowupMode::Guide => resource::FollowupMode::Guide,
                        },
                    )
                    .map_err(ui_error)?;
            }
            NativeUiAction::Stop { session_id, .. } => {
                // Stop 只操作当前已加载的执行事实，不为停止一个空闲会话触发恢复。
                let session = self
                    .inner
                    .runtime
                    .runtime_manager()
                    .get(session_id.clone())
                    .map_err(ui_error)?;
                for turn in session.active_turn_ids().map_err(ui_error)? {
                    self.inner
                        .runtime
                        .cancel_turn(&session_id, turn.as_str())
                        .map_err(ui_error)?;
                }
            }
            NativeUiAction::LoadHistory { session_id, cursor } => {
                let session = self.open_session(&session_id)?;
                projection::conversation(self, &session, Some(cursor))?;
            }
            NativeUiAction::LoadLatest { session_id } => {
                let session = self.open_session(&session_id)?;
                projection::conversation(self, &session, None)?;
            }
            NativeUiAction::ColdRestore { session_id, .. } => {
                self.inner
                    .drafts
                    .cold_restore(&session_id)
                    .map_err(ui_error)?;
                self.open_session(&session_id)?;
                self.ensure_queue_supervisor(&session_id)?;
            }
            NativeUiAction::Rewind {
                session_id,
                target_turn_id,
                operation_id,
            } => {
                crate::native_rewind::apply(
                    Arc::clone(&self.inner.runtime),
                    &self.inner.paths,
                    &session_id,
                    &target_turn_id,
                    &operation_id,
                )
                .await
                .map_err(|error| ui_error_with_code(error.code(), error))?;
            }
            NativeUiAction::Branch {
                session_id,
                through_turn_id,
                operation_id,
            } => {
                let context = crate::session_commands::close_session_for_mutation(
                    &self.inner.runtime,
                    &self.inner.paths,
                    &session_id,
                )
                .await
                .map_err(ui_error)?;
                let request = resource::SessionForkRequest {
                    source_session_id: SessionId::new(session_id.clone()).map_err(ui_error)?,
                    operation_id,
                    title: None,
                    through_turn_id: through_turn_id
                        .map(TurnId::new)
                        .transpose()
                        .map_err(ui_error)?,
                };
                let forked = crate::session_commands::retry_session_mutation(|| {
                    self.inner
                        .runtime
                        .runtime_manager()
                        .fork_closed_session(request.clone())
                })
                .await
                .map_err(ui_error)?;
                crate::session_commands::restore_session_after_mutation(
                    &self.inner.runtime,
                    &session_id,
                    &context,
                )
                .map_err(ui_error)?;
                self.open_session(forked.session_id.as_str())?;
                result_session = Some(forked.session_id.as_str().to_owned());
            }
            NativeUiAction::Feedback {
                session_id,
                message_id,
                feedback,
                expected_transcript_revision,
                operation_id,
            } => {
                let session = self.open_session(&session_id)?;
                let root = session
                    .read_state(|state| state.project_root.clone())
                    .map_err(ui_error)?;
                // 消息 ID 是原生投影稳定身份，摘要只满足 Journal 非零数值键约束。
                let row_id = stable_row_id(&message_id);
                session
                    .set_assistant_feedback(
                        &operation_id,
                        row_id,
                        &message_id,
                        feedback.map(|feedback| match feedback {
                            AssistantFeedback::Like => resource::AssistantFeedback::Like,
                            AssistantFeedback::Dislike => resource::AssistantFeedback::Dislike,
                        }),
                        expected_transcript_revision,
                        &root,
                    )
                    .map_err(ui_error)?;
            }
            NativeUiAction::ApproveTool {
                session_id,
                request_id,
                approved,
                ..
            } => {
                self.respond_permission(&session_id, &request_id, approved)?;
            }
            NativeUiAction::AnswerQuestion {
                session_id,
                request_id,
                answer,
                ..
            } => {
                self.respond_question(&session_id, &request_id, &answer)?;
            }
            NativeUiAction::AddAttachment {
                session_id, path, ..
            } => {
                self.open_session(&session_id)?;
                let attachment = inspect_attachment(&path)?;
                let mut draft = self.draft(&session_id)?;
                if draft.attachments.len() >= 32 {
                    return Err(ui_error("最多添加 32 个附件"));
                }
                if !draft
                    .attachments
                    .iter()
                    .any(|item| item.attachment_id == attachment.attachment_id)
                {
                    draft.attachments.push(attachment);
                }
                self.inner
                    .drafts
                    .set(&session_id, &draft)
                    .map_err(ui_error)?;
                let _ = self
                    .inner
                    .local_changes
                    .send((session_id, Some(NativeUiEvent::DraftChanged(draft))));
            }
            NativeUiAction::RemoveAttachment {
                session_id,
                attachment_id,
            } => {
                let mut draft = self.draft(&session_id)?;
                draft
                    .attachments
                    .retain(|attachment| attachment.attachment_id != attachment_id);
                self.inner
                    .drafts
                    .set(&session_id, &draft)
                    .map_err(ui_error)?;
                let _ = self
                    .inner
                    .local_changes
                    .send((session_id, Some(NativeUiEvent::DraftChanged(draft))));
            }
            NativeUiAction::LoadQueuedInput {
                session_id,
                queue_item_id,
            } => {
                let session = self.open_session(&session_id)?;
                let item = session
                    .read_state(|state| {
                        state
                            .input_queue
                            .items
                            .iter()
                            .find(|item| item.queue_item_id == queue_item_id)
                            .map(projection::full_input_queue_fact)
                    })
                    .map_err(ui_error)?
                    .ok_or_else(|| ui_error("队列项不存在或已经消费"))?;
                // 读取动作只广播当前 ID 的完整正文；后续 SendQueuedInput 仍由
                // send_queued 从 Runtime 再取权威项，不依赖这个可丢弃的 UI 事件。
                let _ = self
                    .inner
                    .local_changes
                    .send((session_id, Some(NativeUiEvent::QueuedInputLoaded(item))));
            }
            NativeUiAction::EditQueuedInput {
                session_id,
                queue_item_id,
                text,
                operation_id,
            } => {
                self.open_session(&session_id)?;
                self.inner
                    .runtime
                    .edit_session_input(&session_id, &operation_id, &queue_item_id, text)
                    .map_err(ui_error)?;
            }
            NativeUiAction::ReorderQueuedInput {
                session_id,
                queue_item_ids,
                operation_id,
            } => {
                let session = self.open_session(&session_id)?;
                let actual = session
                    .read_state(|state| {
                        state
                            .input_queue
                            .items
                            .iter()
                            .map(|item| item.queue_item_id.clone())
                            .collect::<HashSet<_>>()
                    })
                    .map_err(ui_error)?;
                if queue_item_ids.iter().collect::<HashSet<_>>().len() != actual.len()
                    || queue_item_ids.iter().any(|id| !actual.contains(id))
                {
                    return Err(ui_error("队列顺序已改变，请重新加载"));
                }
                let mut before = None;
                for (index, id) in queue_item_ids.iter().enumerate().rev() {
                    self.inner
                        .runtime
                        .reorder_session_input(
                            &session_id,
                            &format!("{operation_id}:{index}"),
                            id,
                            before,
                        )
                        .map_err(ui_error)?;
                    before = Some(id.as_str());
                }
            }
            NativeUiAction::SendQueuedInput {
                session_id,
                queue_item_id,
                operation_id,
            } => {
                self.send_queued(&session_id, &queue_item_id, &operation_id)
                    .await?;
            }
            NativeUiAction::DeleteQueuedInput {
                session_id,
                queue_item_id,
                operation_id,
            } => {
                self.open_session(&session_id)?;
                self.inner
                    .runtime
                    .delete_session_input(&session_id, &operation_id, &queue_item_id)
                    .map_err(ui_error)?;
            }
            NativeUiAction::SelectPromptHistory {
                session_id,
                entry_id,
            } => {
                let session = self.open_session(&session_id)?;
                let text = session
                    .read_state(|state| {
                        state
                            .raw_transcript_messages()
                            .into_iter()
                            .find(|message| {
                                message.message_id == entry_id
                                    && message.role == resource::MessageRole::User
                            })
                            .map(|message| {
                                message
                                    .content
                                    .iter()
                                    .filter_map(|part| {
                                        if let resource::MessagePart::Text { text } = part {
                                            Some(text.as_str())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect::<String>()
                            })
                    })
                    .map_err(ui_error)?
                    .ok_or_else(|| ui_error("输入历史不存在"))?;
                let mut draft = self.draft(&session_id)?;
                draft.text = text;
                self.inner
                    .drafts
                    .set(&session_id, &draft)
                    .map_err(ui_error)?;
                let _ = self
                    .inner
                    .local_changes
                    .send((session_id, Some(NativeUiEvent::DraftChanged(draft))));
            }
        }
        if operation_id.is_empty() {
            operation_id = "native-read".into();
        }
        let accepted_sequence = result_session
            .as_ref()
            .and_then(|id| self.inner.runtime.runtime_manager().get(id.clone()).ok())
            .and_then(|session| session.read_state(|state| state.last_sequence).ok());
        Ok(NativeActionReceipt {
            operation_id,
            session_id: result_session,
            accepted_sequence,
        })
    }

    fn select_turn_options(
        &self,
        session_id: &str,
        operation_id: &str,
        model: Option<&ModelSelection>,
        effort: Option<&str>,
        permission: PermissionMode,
        plan: PlanMode,
    ) -> Result<(), NativeUiError> {
        if let Some(model) = model {
            self.inner
                .runtime
                .set_session_model(
                    session_id,
                    &format!("{operation_id}:model"),
                    &model.provider_id,
                    &model.model,
                )
                .map_err(ui_error)?;
        }
        if let Some(effort) = effort {
            self.inner
                .runtime
                .set_session_effort(session_id, &format!("{operation_id}:effort"), effort)
                .map_err(ui_error)?;
        }
        self.persist_permission(
            session_id,
            &format!("{operation_id}:permission"),
            permission,
        )?;
        self.open_session(session_id)?
            .set_plan(
                &format!("{operation_id}:plan"),
                resource::PlanState {
                    enabled: plan == PlanMode::ReadOnly,
                    ..Default::default()
                },
            )
            .map_err(ui_error)?;
        Ok(())
    }

    pub(super) fn turn_options(
        &self,
        session_id: &str,
        attachments: &[AttachmentFact],
        plan: PlanMode,
    ) -> Result<RootTurnOptions, NativeUiError> {
        let mut options = RootTurnOptions {
            plan_enabled: plan == PlanMode::ReadOnly,
            ..RootTurnOptions::default()
        };
        for attachment in attachments {
            let actual = inspect_attachment(&attachment.path)?;
            if actual.bytes != attachment.bytes || actual.media_type != attachment.media_type {
                return Err(ui_error("附件发送前已经发生变化，请重新添加"));
            }
            let bytes = crate::storage::read_private_bytes_bounded(
                Path::new(&actual.path),
                16 * 1024 * 1024,
                "用户附件",
            )
            .map_err(ui_error)?
            .ok_or_else(|| ui_error("附件不存在"))?;
            if actual.image {
                if !self.session_preferences(session_id).vision_enabled {
                    return Err(ui_error("发送图片前请启用 Vision"));
                }
                options
                    .attachment_images
                    .push(keencode_model::ImageContent::from_base64(
                        actual.media_type,
                        base64::engine::general_purpose::STANDARD.encode(bytes),
                    ));
            } else {
                let text = String::from_utf8(bytes)
                    .map_err(|_| ui_error("当前版本仅支持 UTF-8 文本与图片附件"))?;
                options
                    .attachment_context
                    .push(format!("用户选择的文件：{}\n{}", actual.file_name, text));
            }
        }
        self.bind_native_interactions(session_id, &mut options)?;
        Ok(options)
    }
}

use std::sync::Arc;

pub(super) fn same_path(left: &str, right: &str) -> bool {
    left == right
        || std::fs::canonicalize(left)
            .ok()
            .zip(std::fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

fn inspect_attachment(path: &str) -> Result<AttachmentFact, NativeUiError> {
    let path = std::fs::canonicalize(path).map_err(ui_error)?;
    let metadata = std::fs::metadata(&path).map_err(ui_error)?;
    if !metadata.is_file() || metadata.len() > 16 * 1024 * 1024 {
        return Err(ui_error("附件必须是 16 MiB 以内的文件"));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let media_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "text/plain",
    };
    Ok(AttachmentFact {
        attachment_id: format!("file-{}", stable_row_id(&path.to_string_lossy())),
        file_name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path: path.to_string_lossy().into_owned(),
        media_type: media_type.into(),
        bytes: metadata.len(),
        image: media_type.starts_with("image/"),
    })
}

fn stable_row_id(message_id: &str) -> u64 {
    let hash = Sha256::digest(message_id.as_bytes());
    u64::from_le_bytes(hash[..8].try_into().expect("SHA256 至少包含八字节")).max(1)
}

/// 原生操作 ID 面向 Journal 幂等，可能包含 `:` 等分隔符；资源 Turn ID 则必须
/// 遵守可映射到磁盘的字符集，因此这里用稳定摘要建立两者之间的边界。
fn native_root_turn_id(operation_id: &str) -> String {
    format!("native-root-{:x}", Sha256::digest(operation_id.as_bytes()))
}

fn action_session(action: &NativeUiAction) -> Option<String> {
    match action {
        NativeUiAction::OpenProject { .. }
        | NativeUiAction::LoadWorkspace { .. }
        | NativeUiAction::CreateProject { .. }
        | NativeUiAction::RenameProject { .. }
        | NativeUiAction::ForgetProject { .. }
        | NativeUiAction::ReorderProjects { .. }
        | NativeUiAction::CreateSession { .. }
        | NativeUiAction::GroupSessions { .. } => None,
        NativeUiAction::OpenSession { session_id, .. }
        | NativeUiAction::RenameSession { session_id, .. }
        | NativeUiAction::DeleteSession { session_id, .. }
        | NativeUiAction::SetSessionArchived { session_id, .. }
        | NativeUiAction::SetSessionPinned { session_id, .. }
        | NativeUiAction::SetDraft { session_id, .. }
        | NativeUiAction::Send { session_id, .. }
        | NativeUiAction::SetModel { session_id, .. }
        | NativeUiAction::SetEffort { session_id, .. }
        | NativeUiAction::SetVision { session_id, .. }
        | NativeUiAction::SetPermission { session_id, .. }
        | NativeUiAction::SetPlan { session_id, .. }
        | NativeUiAction::SetFollowupMode { session_id, .. }
        | NativeUiAction::Stop { session_id, .. }
        | NativeUiAction::LoadHistory { session_id, .. }
        | NativeUiAction::LoadLatest { session_id }
        | NativeUiAction::ColdRestore { session_id, .. }
        | NativeUiAction::Rewind { session_id, .. }
        | NativeUiAction::Branch { session_id, .. }
        | NativeUiAction::Feedback { session_id, .. }
        | NativeUiAction::ApproveTool { session_id, .. }
        | NativeUiAction::AnswerQuestion { session_id, .. }
        | NativeUiAction::AddAttachment { session_id, .. }
        | NativeUiAction::RemoveAttachment { session_id, .. }
        | NativeUiAction::LoadQueuedInput { session_id, .. }
        | NativeUiAction::EditQueuedInput { session_id, .. }
        | NativeUiAction::ReorderQueuedInput { session_id, .. }
        | NativeUiAction::SendQueuedInput { session_id, .. }
        | NativeUiAction::DeleteQueuedInput { session_id, .. }
        | NativeUiAction::SelectPromptHistory { session_id, .. } => Some(session_id.clone()),
    }
}

fn operation_id(action: &NativeUiAction) -> String {
    match action {
        NativeUiAction::CreateProject { operation_id, .. }
        | NativeUiAction::RenameProject { operation_id, .. }
        | NativeUiAction::ForgetProject { operation_id, .. }
        | NativeUiAction::ReorderProjects { operation_id, .. }
        | NativeUiAction::CreateSession { operation_id, .. }
        | NativeUiAction::RenameSession { operation_id, .. }
        | NativeUiAction::DeleteSession { operation_id, .. }
        | NativeUiAction::SetSessionArchived { operation_id, .. }
        | NativeUiAction::SetSessionPinned { operation_id, .. }
        | NativeUiAction::GroupSessions { operation_id, .. }
        | NativeUiAction::Send { operation_id, .. }
        | NativeUiAction::SetModel { operation_id, .. }
        | NativeUiAction::SetEffort { operation_id, .. }
        | NativeUiAction::SetVision { operation_id, .. }
        | NativeUiAction::SetPermission { operation_id, .. }
        | NativeUiAction::SetPlan { operation_id, .. }
        | NativeUiAction::SetFollowupMode { operation_id, .. }
        | NativeUiAction::Stop { operation_id, .. }
        | NativeUiAction::ColdRestore { operation_id, .. }
        | NativeUiAction::Rewind { operation_id, .. }
        | NativeUiAction::Branch { operation_id, .. }
        | NativeUiAction::Feedback { operation_id, .. }
        | NativeUiAction::ApproveTool { operation_id, .. }
        | NativeUiAction::AnswerQuestion { operation_id, .. }
        | NativeUiAction::AddAttachment { operation_id, .. }
        | NativeUiAction::EditQueuedInput { operation_id, .. }
        | NativeUiAction::ReorderQueuedInput { operation_id, .. }
        | NativeUiAction::SendQueuedInput { operation_id, .. }
        | NativeUiAction::DeleteQueuedInput { operation_id, .. } => operation_id.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::native_root_turn_id;
    use keencode_resources::TurnId;

    #[test]
    fn native_root_turn_id_accepts_ui_operation_delimiters() {
        let operation_id = "native-ui:send:18db9b2daf428c38-2";
        let turn_id = native_root_turn_id(operation_id);

        assert_eq!(turn_id, native_root_turn_id(operation_id));
        assert!(TurnId::new(turn_id).is_ok());
    }
}
