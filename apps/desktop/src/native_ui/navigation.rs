//! 原生工作台的有限 UI 导航历史。
//!
//! 历史只保存页面身份和稳定资源 ID，不保存草稿正文、附件或会话快照；实际内容
//! 仍由 `NativeHost` 和当前窗口投影恢复。这样前进/后退不会复制一份 64 项的输入状态。

use super::{settings::SettingsPage, workbench::WorkbenchPane};

pub(crate) const MAX_NAVIGATION_ENTRIES: usize = 64;

/// 可由窗口导航恢复的页面身份。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeNavigationTarget {
    ChatSession {
        session_id: String,
        project_root: String,
    },
    ChatDraft {
        draft_id: String,
        project_root: String,
    },
    Settings {
        page: SettingsPage,
    },
    Workbench {
        pane: WorkbenchPane,
        project_root: String,
    },
}

impl NativeNavigationTarget {
    pub(crate) fn chat_session(session_id: impl Into<String>, project_root: &str) -> Self {
        Self::ChatSession {
            session_id: session_id.into(),
            project_root: normalize_project_root(project_root),
        }
    }

    pub(crate) fn chat_draft(draft_id: impl Into<String>, project_root: &str) -> Self {
        Self::ChatDraft {
            draft_id: draft_id.into(),
            project_root: normalize_project_root(project_root),
        }
    }

    pub(crate) fn settings(page: SettingsPage) -> Self {
        Self::Settings { page }
    }

    pub(crate) fn workbench(pane: WorkbenchPane, project_root: &str) -> Self {
        Self::Workbench {
            pane,
            project_root: normalize_project_root(project_root),
        }
    }

    fn normalized(self) -> Self {
        match self {
            Self::ChatSession {
                session_id,
                project_root,
            } => Self::chat_session(session_id, &project_root),
            Self::ChatDraft {
                draft_id,
                project_root,
            } => Self::chat_draft(draft_id, &project_root),
            Self::Settings { page } => Self::settings(page),
            Self::Workbench { pane, project_root } => Self::workbench(pane, &project_root),
        }
    }

    /// 比较稳定页面身份；路径使用与工作区投影相同的 slash/盘符规范化规则。
    fn same_destination(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::ChatSession {
                    session_id: left_session,
                    project_root: left_root,
                },
                Self::ChatSession {
                    session_id: right_session,
                    project_root: right_root,
                },
            ) => {
                left_session == right_session
                    && normalize_project_root(left_root) == normalize_project_root(right_root)
            }
            (
                Self::ChatDraft {
                    draft_id: left_draft,
                    project_root: left_root,
                },
                Self::ChatDraft {
                    draft_id: right_draft,
                    project_root: right_root,
                },
            ) => {
                left_draft == right_draft
                    && normalize_project_root(left_root) == normalize_project_root(right_root)
            }
            (Self::Settings { page: left }, Self::Settings { page: right }) => left == right,
            (
                Self::Workbench {
                    pane: left_pane,
                    project_root: left_root,
                },
                Self::Workbench {
                    pane: right_pane,
                    project_root: right_root,
                },
            ) => {
                left_pane == right_pane
                    && normalize_project_root(left_root) == normalize_project_root(right_root)
            }
            _ => false,
        }
    }

    pub(crate) fn project_root(&self) -> Option<&str> {
        match self {
            Self::ChatSession { project_root, .. }
            | Self::ChatDraft { project_root, .. }
            | Self::Workbench { project_root, .. } => Some(project_root),
            Self::Settings { .. } => None,
        }
    }
}

/// 后退/前进的方向，用于失败目标移除后的继续查找。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NavigationDirection {
    Back,
    Forward,
}

/// 异步目标恢复的 guard；token 失效后，迟到的宿主回执不能覆盖新页面。
#[derive(Clone, Debug)]
pub(crate) struct NavigationTraversal {
    pub(crate) token: u64,
    pub(crate) direction: NavigationDirection,
    pub(crate) target: NativeNavigationTarget,
}

/// 有界 cursor 历史。`index` 始终指向当前页面，用户新导航会截断其后的前进分支。
#[derive(Clone, Debug, Default)]
pub(crate) struct NavigationHistory {
    entries: Vec<NativeNavigationTarget>,
    index: Option<usize>,
}

impl NavigationHistory {
    pub(crate) fn with_initial(target: NativeNavigationTarget) -> Self {
        Self {
            entries: vec![target.normalized()],
            index: Some(0),
        }
    }

    pub(crate) fn current(&self) -> Option<&NativeNavigationTarget> {
        self.index.and_then(|index| self.entries.get(index))
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn can_go_back(&self) -> bool {
        self.index.is_some_and(|index| index > 0)
    }

    pub(crate) fn can_go_forward(&self) -> bool {
        self.index
            .is_some_and(|index| index + 1 < self.entries.len())
    }

    /// 记录用户打开的新页面。相同目标不入栈，且不复制草稿正文或附件。
    pub(crate) fn record_user(&mut self, target: NativeNavigationTarget) -> bool {
        let target = target.normalized();
        if self
            .current()
            .is_some_and(|current| current.same_destination(&target))
        {
            return false;
        }

        if let Some(index) = self.index {
            self.entries.truncate(index.saturating_add(1));
        } else {
            self.entries.clear();
        }
        self.entries.push(target);
        self.index = Some(self.entries.len().saturating_sub(1));

        if self.entries.len() > MAX_NAVIGATION_ENTRIES {
            let remove_count = self.entries.len() - MAX_NAVIGATION_ENTRIES;
            self.entries.drain(..remove_count);
            self.index = self.index.map(|index| index.saturating_sub(remove_count));
        }
        true
    }

    pub(crate) fn move_back(&mut self) -> Option<NativeNavigationTarget> {
        let index = self.index?;
        let next = index.checked_sub(1)?;
        self.index = Some(next);
        self.entries.get(next).cloned()
    }

    pub(crate) fn move_forward(&mut self) -> Option<NativeNavigationTarget> {
        let index = self.index?;
        let next = index
            .checked_add(1)
            .filter(|next| *next < self.entries.len())?;
        self.index = Some(next);
        self.entries.get(next).cloned()
    }

    /// 异步恢复遇到可重试错误时回到原 anchor，保留目标供用户再次尝试。
    pub(crate) fn return_from_traversal(
        &mut self,
        direction: NavigationDirection,
    ) -> Option<NativeNavigationTarget> {
        match direction {
            NavigationDirection::Back => self.move_forward(),
            NavigationDirection::Forward => self.move_back(),
        }
    }

    /// 删除指定条目并尽量保持原当前页面；删除当前项时优先选择同方向的相邻项。
    pub(crate) fn remove_at(&mut self, index: usize) -> bool {
        if index >= self.entries.len() {
            return false;
        }
        let current = self.index;
        self.entries.remove(index);
        if self.entries.is_empty() {
            self.index = None;
            return true;
        }

        self.index = match current {
            None => Some(0),
            Some(current) if index < current => Some(current - 1),
            Some(current) if index == current => Some(current.min(self.entries.len() - 1)),
            Some(current) => Some(current),
        };
        true
    }

    /// 删除异步恢复失败的当前候选，并把 cursor 留在原 anchor，避免继续查找跳过一项。
    pub(crate) fn remove_failed_target(
        &mut self,
        target: &NativeNavigationTarget,
        direction: NavigationDirection,
    ) -> bool {
        let Some(candidate_index) = self.index else {
            return false;
        };
        if !self
            .entries
            .get(candidate_index)
            .is_some_and(|current| current.same_destination(target))
        {
            return false;
        }
        // 使用删除前的 cursor 计算 anchor，不能按目标身份搜索，否则非连续重复目标
        // 会把 cursor 错放到更早的同名条目。
        let anchor_index = match direction {
            NavigationDirection::Back => candidate_index,
            NavigationDirection::Forward => candidate_index.saturating_sub(1),
        };
        let removed = self.remove_at(candidate_index);
        if removed && anchor_index < self.entries.len() {
            self.index = Some(anchor_index);
        }
        removed
    }

    pub(crate) fn remove_sessions(&mut self, session_id: &str) -> usize {
        self.remove_matching(|target| {
            matches!(target, NativeNavigationTarget::ChatSession { session_id: current, .. } if current == session_id)
        })
    }

    pub(crate) fn remove_project(&mut self, project_root: &str) -> usize {
        let project_root = normalize_project_root(project_root);
        self.remove_matching(|target| {
            target
                .project_root()
                .is_some_and(|current| normalize_project_root(current) == project_root)
        })
    }

    fn remove_matching(
        &mut self,
        mut predicate: impl FnMut(&NativeNavigationTarget) -> bool,
    ) -> usize {
        let mut removed = 0;
        let mut index = 0;
        while index < self.entries.len() {
            if predicate(&self.entries[index]) {
                self.remove_at(index);
                removed += 1;
            } else {
                index += 1;
            }
        }
        removed
    }
}

pub(crate) fn normalize_project_root(path: &str) -> String {
    let normalized = crate::path_utils::path_text_to_frontend(path);
    if normalized.len() == 3
        && normalized.as_bytes().get(1) == Some(&b':')
        && normalized.ends_with('/')
    {
        return normalized;
    }
    normalized.trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(name: &str) -> NativeNavigationTarget {
        NativeNavigationTarget::chat_draft("draft", name)
    }

    fn session(id: &str) -> NativeNavigationTarget {
        NativeNavigationTarget::chat_session(id, "D:/project")
    }

    #[test]
    fn duplicate_targets_are_not_recorded() {
        let mut history = NavigationHistory::with_initial(draft("D:/project"));
        assert!(!history.record_user(draft(r"D:\project\")));
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn user_navigation_truncates_forward_branch() {
        let mut history = NavigationHistory::with_initial(draft("D:/project"));
        history.record_user(session("a"));
        history.record_user(session("b"));
        assert_eq!(history.move_back(), Some(session("a")));
        assert!(history.record_user(session("c")));
        assert!(!history.can_go_forward());
        assert_eq!(history.current(), Some(&session("c")));
    }

    #[test]
    fn history_is_bounded_to_sixty_four_entries() {
        let mut history = NavigationHistory::with_initial(draft("D:/project"));
        for index in 0..80 {
            history.record_user(session(&index.to_string()));
        }
        assert_eq!(history.len(), MAX_NAVIGATION_ENTRIES);
        assert_eq!(history.current(), Some(&session("79")));
        for _ in 0..(MAX_NAVIGATION_ENTRIES - 1) {
            history.move_back();
        }
        assert_eq!(history.current(), Some(&session("16")));
        assert!(!history.can_go_back());
    }

    #[test]
    fn removing_back_target_preserves_current_anchor() {
        let mut history = NavigationHistory::with_initial(draft("D:/project"));
        history.record_user(session("a"));
        history.record_user(session("b"));
        history.move_back();
        assert_eq!(history.current(), Some(&session("a")));
        history.remove_failed_target(&session("a"), NavigationDirection::Back);
        assert_eq!(history.current(), Some(&session("b")));
        assert!(!history.can_go_forward());
    }

    #[test]
    fn removing_historical_target_keeps_current_target() {
        let mut history = NavigationHistory::with_initial(draft("D:/project"));
        history.record_user(session("a"));
        history.record_user(session("b"));
        assert_eq!(history.move_back(), Some(session("a")));
        history.remove_at(0);
        assert_eq!(history.current(), Some(&session("a")));
        assert_eq!(history.move_forward(), Some(session("b")));
    }

    #[test]
    fn deleting_sessions_and_projects_removes_all_matching_entries() {
        let mut history = NavigationHistory::with_initial(draft("D:/one"));
        history.record_user(session("a"));
        history.record_user(NativeNavigationTarget::chat_session("b", "D:/two"));
        history.record_user(NativeNavigationTarget::workbench(
            WorkbenchPane::Files,
            "D:/one",
        ));
        assert_eq!(history.remove_sessions("a"), 1);
        assert_eq!(history.remove_project("D:/one"), 2);
        assert_eq!(history.len(), 1);
        assert_eq!(
            history.current(),
            Some(&NativeNavigationTarget::chat_session("b", "D:/two"))
        );
    }

    #[test]
    fn failed_forward_target_can_be_removed_before_trying_next_target() {
        let mut history = NavigationHistory::with_initial(draft("D:/project"));
        history.record_user(session("a"));
        history.record_user(session("b"));
        history.move_back();
        let failed = history.move_forward().expect("forward candidate");
        history.remove_failed_target(&failed, NavigationDirection::Forward);
        assert_eq!(history.current(), Some(&session("a")));
        assert!(!history.can_go_forward());
    }

    #[test]
    fn failed_forward_target_keeps_anchor_before_trying_next_target() {
        let mut history = NavigationHistory::with_initial(draft("D:/project"));
        history.record_user(session("a"));
        history.record_user(session("b"));
        history.record_user(session("c"));
        history.move_back();
        history.move_back();
        assert_eq!(history.current(), Some(&session("a")));
        let failed = history.move_forward().expect("failed forward candidate");
        assert!(history.remove_failed_target(&failed, NavigationDirection::Forward));
        assert_eq!(history.current(), Some(&session("a")));
        assert_eq!(history.move_forward(), Some(session("c")));
    }

    #[test]
    fn failed_last_forward_target_keeps_anchor_without_going_past_it() {
        let mut history = NavigationHistory::with_initial(draft("D:/project"));
        history.record_user(session("a"));
        history.record_user(session("b"));
        history.move_back();
        assert_eq!(history.current(), Some(&session("a")));
        let failed = history.move_forward().expect("failed forward candidate");
        assert!(history.remove_failed_target(&failed, NavigationDirection::Forward));
        assert_eq!(history.current(), Some(&session("a")));
        assert!(!history.can_go_forward());
        assert_eq!(history.move_back(), Some(draft("D:/project")));
    }

    #[test]
    fn failed_target_restores_the_indexed_anchor_when_identity_repeats() {
        let mut history = NavigationHistory::with_initial(draft("D:/project"));
        history.record_user(NativeNavigationTarget::settings(SettingsPage::General));
        history.record_user(draft("D:/project"));
        history.record_user(session("failed"));
        history.move_back();
        let failed = history.move_forward().expect("failed forward candidate");
        assert!(history.remove_failed_target(&failed, NavigationDirection::Forward));
        assert_eq!(history.current(), Some(&draft("D:/project")));
        assert_eq!(
            history.move_back(),
            Some(NativeNavigationTarget::settings(SettingsPage::General))
        );
    }

    #[test]
    fn project_roots_use_the_same_path_identity_as_workspace() {
        assert_eq!(normalize_project_root(r"\\?\D:\project\"), "D:/project");
        assert_eq!(normalize_project_root("D:/"), "D:/");
    }
}
