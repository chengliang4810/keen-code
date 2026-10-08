//! Native 工作台侧栏：项目和 Session 只从宿主提供的当前页投影绘制。

use std::{collections::HashSet, rc::Rc};

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    forms::{Choice, Combobox, Input, TextInput},
    menus::{ContextMenu, Menu, MenuItem, OverflowMenu},
    primitives::{FocusRing, Icon, IconName, Tooltip},
    theme::{ActiveTheme, ControlSize, IconSize},
};
use gpui::{
    AnyElement, App, IntoElement, MouseButton, ParentElement, RenderOnce, Role, Styled, Window,
    WindowControlArea, div, img, prelude::*, px, transparent_black,
};
use sha2::{Digest, Sha256};

use super::{
    model::{GroupFact, NativeUiAction, ProjectFact, SessionFact, SessionStatus, WorkspacePage},
    settings::{NativeKeybindingAction, NativeKeybindingsState, SettingsPage},
    style::{SIDEBAR_WIDTH, ShellColors, UiTextSize, ui_text_size},
};

type ActionHandler = Rc<dyn Fn(NativeUiAction, &mut Window, &mut App)>;
type WorkbenchHandler = Rc<dyn Fn(&mut Window, &mut App)>;
type SettingsHandler = Rc<dyn Fn(SettingsPage, &mut Window, &mut App)>;

/// 来源导航行的 h-8、gap-2 与左右留白独立于 Ely 默认居中按钮布局。
/// 保留 Ely 焦点语义和真实回调，键盘与鼠标走同一个操作。
fn navigation_row(
    id: &'static str,
    label: &'static str,
    icon: Icon,
    shortcut: Option<&str>,
    handler: WorkbenchHandler,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let key_handler = handler.clone();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .tab_index(0)
        .w_full()
        .h(px(32.0))
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(transparent_black())
        .text_size(ui_text_size(theme, UiTextSize::Base))
        .font_weight(gpui::FontWeight::NORMAL)
        .text_color(colors.fg)
        .cursor_pointer()
        .hover(|style| style.bg(colors.hover))
        .active(|style| style.bg(colors.active))
        .focus_ring(cx)
        .child(icon.size(IconSize::Md))
        .child(div().flex_1().min_w_0().child(label))
        .when_some(shortcut, |row, shortcut| {
            row.child(
                div()
                    .flex_none()
                    .text_size(ui_text_size(theme, UiTextSize::Xs))
                    .text_color(colors.fg_subtle)
                    .child(shortcut.to_owned()),
            )
        })
        .on_click(move |_, window, cx| handler(window, cx))
        .on_key_down(move |event, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                cx.stop_propagation();
                key_handler(window, cx);
            }
        })
        .into_any_element()
}

fn navigation_history_button(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    enabled: bool,
    handler: WorkbenchHandler,
    cx: &App,
) -> AnyElement {
    let colors = cx.theme().colors.clone();
    if enabled {
        let key_handler = handler.clone();
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label)
            // Windows 非客户区命中会先查 Drag hitbox；按钮必须遮挡身后的拖动层，
            // 仅阻止鼠标默认行为不能把 HTCAPTION 重新变成真实按钮点击。
            .occlude()
            .tab_index(0)
            .size(px(28.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.0))
            .text_color(colors.fg)
            .cursor_pointer()
            .hover(|style| style.bg(colors.hover))
            .active(|style| style.bg(colors.active))
            .focus_ring(cx)
            .tooltip(Tooltip::text(label))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(move |_, window, cx| handler(window, cx))
            .on_key_down(move |event, window, cx| {
                if !event.keystroke.modifiers.modified()
                    && matches!(event.keystroke.key.as_str(), "enter" | "space")
                {
                    cx.stop_propagation();
                    key_handler(window, cx);
                }
            })
            .child(Icon::new(icon).size(IconSize::Md))
            .into_any_element()
    } else {
        // GPUI 当前没有 aria_disabled 元素 API；禁用态保持来源按钮的 28px 几何，
        // 只保留稳定的 Button/label 节点，不注册焦点、鼠标或键盘处理器。
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label)
            .occlude()
            .size(px(28.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.0))
            .text_color(colors.fg_muted)
            .opacity(0.45)
            .cursor_not_allowed()
            .tooltip(Tooltip::text(label))
            .child(Icon::new(icon).size(IconSize::Md))
            .into_any_element()
    }
}

struct SessionRowContext<'a> {
    project_root: String,
    groups: &'a [GroupFact],
    group_revision: u64,
    group_choices: &'a [Choice],
}

#[derive(Clone, Copy)]
struct SidebarListOptions {
    grouped: bool,
    archived: bool,
    alphabetical: bool,
    collapsed: bool,
}

/// 侧栏的局部投影。动作回调由 `NativeUi` 绑定宿主，不在组件内伪造结果。
#[derive(IntoElement)]
pub struct Sidebar {
    root_path: Option<String>,
    workspace: Option<WorkspacePage>,
    active_session: Option<String>,
    keybindings: NativeKeybindingsState,
    on_action: ActionHandler,
    on_workbench: Option<WorkbenchHandler>,
    on_search: Option<WorkbenchHandler>,
    on_settings: Option<SettingsHandler>,
    navigation_back_enabled: bool,
    navigation_forward_enabled: bool,
    on_navigation_back: Option<WorkbenchHandler>,
    on_navigation_forward: Option<WorkbenchHandler>,
}

impl Sidebar {
    pub fn new(
        root_path: Option<String>,
        workspace: Option<WorkspacePage>,
        active_session: Option<String>,
        keybindings: NativeKeybindingsState,
        on_action: impl Fn(NativeUiAction, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            root_path,
            workspace,
            active_session,
            keybindings,
            on_action: Rc::new(on_action),
            on_workbench: None,
            on_search: None,
            on_settings: None,
            navigation_back_enabled: false,
            navigation_forward_enabled: false,
            on_navigation_back: None,
            on_navigation_forward: None,
        }
    }

    /// 由 gpui_chat 根组件绑定右侧 NativeWorkbench 面板；不把工作台动作伪装成
    /// Session Journal action，面板状态由工作台服务单独维护。
    pub fn with_workbench_handler(
        mut self,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_workbench = Some(Rc::new(handler));
        self
    }

    pub fn with_search_handler(
        mut self,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_search = Some(Rc::new(handler));
        self
    }

    /// 导航只选择已经装配的设置页，由设置实体负责读取与订阅宿主状态。
    pub fn with_settings_handler(
        mut self,
        handler: impl Fn(SettingsPage, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_settings = Some(Rc::new(handler));
        self
    }

    /// 顶栏历史按钮固定保留 28px 命中面；拖拽区只放在其后的空白区域。
    pub fn with_navigation_handlers(
        mut self,
        back_enabled: bool,
        forward_enabled: bool,
        back: impl Fn(&mut Window, &mut App) + 'static,
        forward: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.navigation_back_enabled = back_enabled;
        self.navigation_forward_enabled = forward_enabled;
        self.on_navigation_back = Some(Rc::new(back));
        self.on_navigation_forward = Some(Rc::new(forward));
        self
    }

    fn project_row(
        &self,
        project: &ProjectFact,
        workspace: &WorkspacePage,
        project_index: usize,
        options: SidebarListOptions,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let (colors, project_text_size, group_text_size) = {
            let theme = cx.theme();
            (
                theme.colors.clone(),
                ui_text_size(theme, UiTextSize::Base),
                ui_text_size(theme, UiTextSize::Sm),
            )
        };
        let project_key = project.project_key.clone();
        let project_count = workspace.projects.len();
        let project_order = workspace
            .projects
            .iter()
            .map(|project| project.root_path.clone())
            .collect::<Vec<_>>();
        let mut sessions = workspace
            .sessions
            .iter()
            .filter(|session| {
                session.project_key == project.project_key && session.archived == options.archived
            })
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            right
                .pinned
                .cmp(&left.pinned)
                .then_with(|| {
                    if options.alphabetical {
                        left.title.cmp(&right.title)
                    } else {
                        right.updated_at_unix_ms.cmp(&left.updated_at_unix_ms)
                    }
                })
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        let no_sessions = sessions.is_empty();
        let action = self.on_action.clone();
        let root = project.root_path.clone();
        let header_root = root.clone();
        let header_project_key = project_key.clone();
        let project_group = format!("native-project-row:{project_key}");
        let project_name_field =
            window.use_keyed_state(format!("native-project-name:{project_key}"), cx, {
                let name = project.display_name.clone();
                move |window, cx| {
                    let mut field = TextInput::new(window, cx).placeholder("项目名称");
                    field.set_text(name, cx);
                    field
                }
            });
        let project_editing = window.use_keyed_state(
            format!("native-project-editing:{project_key}"),
            cx,
            |_, _| false,
        );
        let project_header = if *project_editing.read(cx) {
            let save_action = self.on_action.clone();
            let save_field = project_name_field.clone();
            let save_editing = project_editing.clone();
            let save_root = root.clone();
            let cancel_editing = project_editing.clone();
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .child(Icon::new(IconName::Folder).size(IconSize::Xs))
                .child(Input::new(&project_name_field).size(ControlSize::Sm))
                .child(
                    IconButton::new(format!("project-save:{project_key}"), IconName::Check)
                        .variant(ButtonVariant::Subtle)
                        .size(ControlSize::Sm)
                        .tooltip("保存项目名称")
                        .on_click(move |_, window, cx| {
                            let name = save_field.read(cx).text().trim().to_owned();
                            if name.is_empty() {
                                return;
                            }
                            save_action(
                                NativeUiAction::RenameProject {
                                    project_root: save_root.clone(),
                                    name,
                                    operation_id: format!("ui:project-rename:{save_root}"),
                                },
                                window,
                                cx,
                            );
                            save_editing.update(cx, |editing, cx| {
                                *editing = false;
                                cx.notify();
                            });
                        }),
                )
                .child(
                    IconButton::new(format!("project-cancel:{project_key}"), IconName::X)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("取消重命名")
                        .on_click(move |_, _, cx| {
                            cancel_editing.update(cx, |editing, cx| {
                                *editing = false;
                                cx.notify();
                            });
                        }),
                )
                .into_any_element()
        } else {
            let select_action = self.on_action.clone();
            let select_root = root.clone();
            let project_title = div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(project.display_name.clone())
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    select_action(
                        NativeUiAction::OpenProject {
                            project_root: select_root.clone(),
                        },
                        window,
                        cx,
                    )
                });
            let edit_field = project_name_field.clone();
            let edit_name = project.display_name.clone();
            let edit_editing = project_editing.clone();
            let forget_action = self.on_action.clone();
            let forget_root = root.clone();
            let mut project_actions = div().flex().items_center().gap_0p5();
            if project_index > 0 {
                let move_action = self.on_action.clone();
                let mut order = project_order.clone();
                order.swap(project_index - 1, project_index);
                let move_operation_key = project_key.clone();
                project_actions = project_actions.child(
                    IconButton::new(format!("project-up:{project_key}"), IconName::ArrowUp)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("上移项目")
                        .on_click(move |_, window, cx| {
                            move_action(
                                NativeUiAction::ReorderProjects {
                                    project_roots: order.clone(),
                                    operation_id: format!("ui:project-up:{move_operation_key}"),
                                },
                                window,
                                cx,
                            )
                        }),
                );
            }
            if project_index + 1 < project_count {
                let move_action = self.on_action.clone();
                let mut order = project_order.clone();
                order.swap(project_index, project_index + 1);
                let move_operation_key = project_key.clone();
                project_actions = project_actions.child(
                    IconButton::new(format!("project-down:{project_key}"), IconName::ArrowDown)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("下移项目")
                        .on_click(move |_, window, cx| {
                            move_action(
                                NativeUiAction::ReorderProjects {
                                    project_roots: order.clone(),
                                    operation_id: format!("ui:project-down:{move_operation_key}"),
                                },
                                window,
                                cx,
                            )
                        }),
                );
            }
            project_actions = project_actions
                .child(
                    IconButton::new(format!("project-rename:{project_key}"), IconName::Pencil)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("重命名项目")
                        .on_click(move |_, _, cx| {
                            edit_field.update(cx, |field, cx| {
                                field.set_text(edit_name.clone(), cx);
                            });
                            edit_editing.update(cx, |editing, cx| {
                                *editing = true;
                                cx.notify();
                            });
                        }),
                )
                .child(
                    IconButton::new(format!("project-forget:{project_key}"), IconName::Trash2)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("移除项目（保留文件）")
                        .on_click(move |_, window, cx| {
                            forget_action(
                                NativeUiAction::ForgetProject {
                                    project_root: forget_root.clone(),
                                    operation_id: format!("ui:project-forget:{forget_root}"),
                                },
                                window,
                                cx,
                            )
                        }),
                );
            project_actions = project_actions
                .child(
                    IconButton::new(format!("project-new:{project_key}"), IconName::Plus)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("新建会话")
                        .on_click(move |_, window, cx| {
                            action(
                                NativeUiAction::CreateSession {
                                    project_root: header_root.clone(),
                                    operation_id: format!("ui:new-session:{}", header_project_key),
                                },
                                window,
                                cx,
                            )
                        }),
                )
                .absolute()
                .right_0()
                .h_full()
                .bg(ShellColors::from_theme(cx.theme()).sidebar)
                // 操作浮于文字上方；隐藏时不挤占标题宽度，也不增加来源的 32px 行高。
                .opacity(0.0)
                .group_hover(project_group.clone(), |style| style.opacity(1.0));
            div()
                .group(project_group.clone())
                .relative()
                .flex()
                .items_center()
                .h(px(32.0))
                .gap_2()
                .px_2()
                .text_size(project_text_size)
                .text_color(colors.fg_muted)
                .child(
                    Icon::new(IconName::Folder)
                        .size(IconSize::Sm)
                        .color(colors.fg_subtle),
                )
                .child(project_title)
                .child(project_actions)
                .into_any_element()
        };
        let groups = workspace
            .groups
            .iter()
            .filter(|group| group.project_key == project.project_key)
            .cloned()
            .collect::<Vec<_>>();
        let group_revision = project.group_revision;
        let group_choices = group_choices(&groups);
        let mut grouped_session_ids = HashSet::new();
        let mut rows = Vec::new();
        for group in groups.iter().filter(|_| options.grouped) {
            let group_sessions = sessions
                .iter()
                .filter(|session| group.session_ids.iter().any(|id| id == &session.session_id))
                .cloned()
                .collect::<Vec<_>>();
            for session in &group_sessions {
                grouped_session_ids.insert(session.session_id.clone());
            }
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .pt_2()
                    .text_size(group_text_size)
                    .text_color(colors.fg_subtle)
                    .child(Icon::new(IconName::FolderOpen).size(IconSize::Xs))
                    .child(group.title.clone())
                    .into_any_element(),
            );
            for session in group_sessions {
                rows.push(self.session_row(
                    session,
                    SessionRowContext {
                        project_root: root.clone(),
                        groups: &groups,
                        group_revision,
                        group_choices: &group_choices,
                    },
                    window,
                    cx,
                ));
            }
        }
        for session in sessions
            .into_iter()
            .filter(|session| !grouped_session_ids.contains(&session.session_id))
        {
            rows.push(self.session_row(
                session,
                SessionRowContext {
                    project_root: root.clone(),
                    groups: &groups,
                    group_revision,
                    group_choices: &group_choices,
                },
                window,
                cx,
            ));
        }
        div()
            .group(project_group)
            .flex()
            .flex_col()
            .gap_0p5()
            .child(project_header)
            .when(no_sessions && !options.collapsed, |column| {
                column.child(
                    div()
                        .pl(px(32.0))
                        .py_3()
                        .text_size(project_text_size)
                        .text_color(colors.fg_subtle)
                        .child(if options.archived {
                            "暂无归档任务"
                        } else {
                            "暂无任务"
                        }),
                )
            })
            .when(!options.collapsed, |column| column.children(rows))
            .into_any_element()
    }

    fn session_row(
        &self,
        session: SessionFact,
        context: SessionRowContext<'_>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let SessionRowContext {
            project_root,
            groups,
            group_revision,
            group_choices,
        } = context;
        let session_id = session.session_id.clone();
        let current_group = groups
            .iter()
            .find(|group| group.session_ids.iter().any(|member| member == &session_id));
        let current_group_id = current_group
            .map(|group| group.group_id.clone())
            .unwrap_or_default();
        let current_group_label = current_group
            .map(|group| group.title.clone())
            .unwrap_or_default();
        let active = self.active_session.as_deref() == Some(session_id.as_str());
        let session_group = format!("native-session-row:{session_id}");
        let selected_action = self.on_action.clone();
        let key_selected_action = selected_action.clone();
        let key_session_id = session_id.clone();
        let key_project_root = project_root.clone();
        let title = if session.title.trim().is_empty() {
            "新对话".to_owned()
        } else {
            session.title.clone()
        };
        let title_field =
            window.use_keyed_state(format!("native-session-title:{session_id}"), cx, {
                let title = title.clone();
                move |window, cx| {
                    let mut field = TextInput::new(window, cx).placeholder("会话标题");
                    field.set_text(title, cx);
                    field
                }
            });
        let editing = window.use_keyed_state(
            format!("native-session-editing:{session_id}"),
            cx,
            |_, _| false,
        );
        if *editing.read(cx) {
            let save_action = self.on_action.clone();
            let save_field = title_field.clone();
            let save_editing = editing.clone();
            let save_session = session_id.clone();
            let cancel_editing = editing.clone();
            return div()
                .id(format!("session-editor:{session_id}"))
                .flex()
                .items_center()
                .gap_1()
                .p_1()
                .child(Input::new(&title_field).size(ControlSize::Sm))
                .child(
                    IconButton::new(format!("session-save:{session_id}"), IconName::Check)
                        .variant(ButtonVariant::Subtle)
                        .size(ControlSize::Sm)
                        .tooltip("保存名称")
                        .on_click(move |_, window, cx| {
                            let title = save_field.read(cx).text().trim().to_owned();
                            if title.is_empty() {
                                return;
                            }
                            save_action(
                                NativeUiAction::RenameSession {
                                    session_id: save_session.clone(),
                                    title,
                                    operation_id: format!("ui:rename:{}", save_session),
                                },
                                window,
                                cx,
                            );
                            save_editing.update(cx, |editing, cx| {
                                *editing = false;
                                cx.notify();
                            });
                        }),
                )
                .child(
                    IconButton::new(format!("session-cancel:{session_id}"), IconName::X)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("取消重命名")
                        .on_click(move |_, _, cx| {
                            cancel_editing.update(cx, |editing, cx| {
                                *editing = false;
                                cx.notify();
                            });
                        }),
                )
                .into_any_element();
        }
        let status = match session.status {
            SessionStatus::Running => "运行中",
            SessionStatus::Waiting => "等待输入",
            SessionStatus::Queued => "排队中",
            SessionStatus::Failed => "失败",
            SessionStatus::Interrupted => "已停止",
            SessionStatus::Corrupt => "需恢复",
            SessionStatus::Completed | SessionStatus::Idle => "",
        };
        let detail = if status.is_empty() {
            None
        } else {
            Some(status.to_owned())
        };
        let rename_field = title_field.clone();
        let rename_editing = editing.clone();
        let rename_title = title.clone();
        let pin_action = self.on_action.clone();
        let pin_session = session_id.clone();
        let archive_action = self.on_action.clone();
        let archive_session = session_id.clone();
        let delete_action = self.on_action.clone();
        let delete_session = session_id.clone();
        let group_field =
            window.use_keyed_state(format!("native-session-group:{session_id}"), cx, {
                let current_group_label = current_group_label.clone();
                move |window, cx| {
                    let mut field = TextInput::new(window, cx).placeholder("分组");
                    field.set_text(current_group_label.clone(), cx);
                    field
                }
            });
        let group_action = self.on_action.clone();
        let group_project_key = project_root.clone();
        let group_session_id = session_id.clone();
        let apply_group_action = self.on_action.clone();
        let apply_group_project_key = project_root.clone();
        let apply_group_session_id = session_id.clone();
        let apply_group_field = group_field.clone();
        let apply_group_choices = group_choices.to_vec();
        let group_picker = Combobox::new(
            format!("session-group:{session_id}"),
            &group_field,
            group_choices.iter().cloned(),
        )
        .selected(current_group_id)
        .size(ControlSize::Sm)
        .free()
        .on_change(move |group_id, window, cx| {
            group_action(
                NativeUiAction::GroupSessions {
                    project_key: group_project_key.clone(),
                    group_id: group_id.to_string(),
                    session_ids: vec![group_session_id.clone()],
                    expected_revision: group_revision,
                    operation_id: grouping_operation_id(
                        &group_session_id,
                        group_id,
                        group_revision,
                    ),
                },
                window,
                cx,
            );
        });
        let apply_group =
            IconButton::new(format!("session-group-apply:{session_id}"), IconName::Check)
                .variant(ButtonVariant::Subtle)
                .size(ControlSize::Sm)
                .tooltip("应用分组")
                .on_click(move |_, window, cx| {
                    let typed = apply_group_field.read(cx).text().trim().to_owned();
                    if typed.is_empty() {
                        return;
                    }
                    let group_id = group_choices_to_id(&apply_group_choices, &typed);
                    apply_group_action(
                        NativeUiAction::GroupSessions {
                            project_key: apply_group_project_key.clone(),
                            group_id: group_id.clone(),
                            session_ids: vec![apply_group_session_id.clone()],
                            expected_revision: group_revision,
                            operation_id: grouping_operation_id(
                                &apply_group_session_id,
                                &group_id,
                                group_revision,
                            ),
                        },
                        window,
                        cx,
                    );
                });
        let mut trailing = div().flex().items_center().gap_0p5();
        trailing = trailing
            .child(div().w(px(80.0)).child(group_picker))
            .child(apply_group)
            // 分组编辑仍占据稳定宽度，默认收起；悬停会话行时再显示。
            .opacity(0.0)
            .group_hover(session_group.clone(), |style| style.opacity(1.0));
        let rename_menu_field = rename_field.clone();
        let rename_menu_editing = rename_editing.clone();
        let rename_menu_title = rename_title.clone();
        let rename_menu = MenuItem::new("重命名")
            .icon(IconName::Pencil)
            .on_click(move |_, cx| {
                rename_menu_field.update(cx, |field, cx| {
                    field.set_text(rename_menu_title.clone(), cx)
                });
                rename_menu_editing.update(cx, |editing, cx| {
                    *editing = true;
                    cx.notify();
                });
            });
        let pin_menu = MenuItem::new(if session.pinned {
            "取消置顶"
        } else {
            "置顶"
        })
        .icon(if session.pinned {
            IconName::PinOff
        } else {
            IconName::Pin
        })
        .on_click(move |window, cx| {
            pin_action(
                NativeUiAction::SetSessionPinned {
                    session_id: pin_session.clone(),
                    pinned: !session.pinned,
                    operation_id: format!("ui:pin:{}", pin_session),
                },
                window,
                cx,
            )
        });
        let archived = session.archived;
        let archive_menu = MenuItem::new(if archived { "取消归档" } else { "归档" })
            .icon(IconName::Archive)
            .on_click(move |window, cx| {
                archive_action(
                    NativeUiAction::SetSessionArchived {
                        session_id: archive_session.clone(),
                        archived: !archived,
                        operation_id: format!("ui:archive:{}", archive_session),
                    },
                    window,
                    cx,
                )
            });
        let delete_menu =
            MenuItem::new("删除")
                .icon(IconName::Trash2)
                .on_click(move |window, cx| {
                    delete_action(
                        NativeUiAction::DeleteSession {
                            session_id: delete_session.clone(),
                            operation_id: format!("ui:delete:{}", delete_session),
                        },
                        window,
                        cx,
                    )
                });
        let session_menu = Menu::new()
            .item(rename_menu)
            .item(pin_menu)
            .item(archive_menu)
            .separator()
            .item(delete_menu);
        let session_menu = div()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                OverflowMenu::new(format!("session-actions:{session_id}"), session_menu)
                    .tooltip("会话操作"),
            );
        let trailing = div()
            .flex()
            .items_center()
            .gap_0p5()
            .when(session.unread, |trailing| {
                trailing.child(
                    Icon::new(IconName::Circle)
                        .size(IconSize::Xs)
                        .color(cx.theme().colors.accent),
                )
            })
            .child(trailing)
            .child(session_menu);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let row = div()
            .id(format!("session:{session_id}"))
            .group(session_group.clone())
            .role(Role::Button)
            .aria_label(title.clone())
            .tab_index(0)
            .relative()
            .flex()
            .items_center()
            .h(px(32.0))
            .pl(px(32.0))
            .pr_2()
            .gap_2()
            .rounded(px(8.0))
            .border_1()
            .border_color(transparent_black())
            .text_size(ui_text_size(theme, UiTextSize::Base))
            .text_color(colors.fg)
            .when(active, |row| row.bg(colors.active))
            .when(session.unread, |row| {
                row.font_weight(gpui::FontWeight::MEDIUM)
            })
            .hover(|row| row.bg(colors.hover))
            .active(|row| row.bg(colors.active))
            .focus_ring(cx)
            .cursor_pointer()
            .when(session.unread, |row| {
                row.child(div().size(px(4.0)).rounded_full().bg(colors.accent))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(title),
            )
            .when_some(detail, |row, detail| {
                row.child(
                    div()
                        .text_size(ui_text_size(theme, UiTextSize::Xs))
                        .text_color(colors.fg_subtle)
                        .child(detail),
                )
            })
            .child(
                div()
                    .absolute()
                    .right_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .bg(if active {
                        colors.active
                    } else {
                        ShellColors::from_theme(theme).sidebar
                    })
                    .opacity(0.0)
                    .group_hover(session_group, |style| style.opacity(1.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(trailing),
            )
            .on_click(move |_, window, cx| {
                selected_action(
                    NativeUiAction::OpenSession {
                        session_id: session_id.clone(),
                        project_root: project_root.clone(),
                    },
                    window,
                    cx,
                )
            })
            .on_key_down(move |event, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    key_selected_action(
                        NativeUiAction::OpenSession {
                            session_id: key_session_id.clone(),
                            project_root: key_project_root.clone(),
                        },
                        window,
                        cx,
                    );
                }
            });
        row.into_any_element()
    }
}

fn group_choices(groups: &[GroupFact]) -> Vec<Choice> {
    let mut choices = vec![Choice::new("", "未分组").icon(IconName::FolderOpen)];
    choices.extend(groups.iter().map(|group| {
        Choice::new(group.group_id.clone(), group.title.clone()).icon(IconName::Folder)
    }));
    choices
}

fn group_choices_to_id(choices: &[Choice], typed: &str) -> String {
    choices
        .iter()
        .find(|choice| choice.label.as_ref() == typed)
        .map(|choice| choice.value.to_string())
        .unwrap_or_else(|| typed.to_owned())
}

/// 用有界且跨进程稳定的 ID 标识一次分组提交，避免用户可控的会话/分组名称
/// 直接把宿主 operation ID 推过 256 字节限制，也避免同一 revision 的重试重复推进。
fn grouping_operation_id(session_id: &str, group_id: &str, revision: u64) -> String {
    let mut digest = Sha256::new();
    digest.update(session_id.as_bytes());
    digest.update([0]);
    digest.update(group_id.as_bytes());
    digest.update([0]);
    digest.update(revision.to_le_bytes());
    let digest = digest.finalize();
    format!(
        "ui:group:{}:{}",
        revision,
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

impl RenderOnce for Sidebar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (colors, shell, tab_text_size) = {
            let theme = cx.theme();
            (
                theme.colors.clone(),
                ShellColors::from_theme(theme),
                ui_text_size(theme, UiTextSize::Sm),
            )
        };
        let workspace = self.workspace.clone();
        let root_path = self.root_path.clone();
        let action = self.on_action.clone();
        let new_action = action.clone();
        let new_chat_shortcut = self.keybindings.display(NativeKeybindingAction::NewChat);
        let workbench = self.on_workbench.clone();
        let search = self.on_search.clone();
        let settings = self.on_settings.clone();
        let navigation_back = self.on_navigation_back.clone();
        let navigation_forward = self.on_navigation_forward.clone();
        let navigation_back_enabled = self.navigation_back_enabled;
        let navigation_forward_enabled = self.navigation_forward_enabled;
        // 排列方式只改变已加载页的显示；归档开关则重新读取宿主，不能从活动页推断归档事实。
        let list_options = window.use_keyed_state("native-sidebar-list-options", cx, |_, _| {
            SidebarListOptions {
                grouped: false,
                archived: false,
                alphabetical: false,
                collapsed: false,
            }
        });
        let options = *list_options.read(cx);
        let grouped_options = list_options.clone();
        let project_options = list_options.clone();
        let archived_options = list_options.clone();
        let archive_action = self.on_action.clone();
        let archive_root = self.root_path.clone();
        let recent_options = list_options.clone();
        let alphabetical_options = list_options.clone();
        let collapse_options = list_options.clone();
        let sort_request = window.use_keyed_state("native-sidebar-sort-request", cx, |_, _| {
            None::<(u64, gpui::Point<gpui::Pixels>)>
        });
        let sort_request_value = *sort_request.read(cx);
        let sort_mouse_request = sort_request.clone();
        let sort_key_request = sort_request.clone();
        let load_more = workspace.as_ref().and_then(|page| page.next.clone());
        let add_project_open = window.use_keyed_state("native-project-add-open", cx, |_, _| false);
        let project_path_field = window.use_keyed_state("native-project-path", cx, |window, cx| {
            TextInput::new(window, cx).placeholder("目录路径（留空使用默认目录）")
        });
        let project_name_field =
            window.use_keyed_state("native-project-name-new", cx, |window, cx| {
                TextInput::new(window, cx).placeholder("项目名称")
            });
        let add_project_panel = if *add_project_open.read(cx) {
            let submit_action = action.clone();
            let submit_path = project_path_field.clone();
            let submit_name = project_name_field.clone();
            let close_form = add_project_open.clone();
            let cancel_form = add_project_open.clone();
            div()
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .py_2()
                .border_b_1()
                .border_color(colors.border)
                .child(Input::new(&project_name_field).size(ControlSize::Sm))
                .child(Input::new(&project_path_field).size(ControlSize::Sm))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Button::new("create-project", "添加")
                                .variant(ButtonVariant::Primary)
                                .size(ControlSize::Sm)
                                .icon(IconName::Plus)
                                .on_click(move |_, window, cx| {
                                    let name = submit_name.read(cx).text().trim().to_owned();
                                    if name.is_empty() {
                                        return;
                                    }
                                    let path = submit_path.read(cx).text().trim().to_owned();
                                    submit_action(
                                        NativeUiAction::CreateProject {
                                            path: (!path.is_empty()).then_some(path),
                                            name,
                                            create_workspace_root_if_missing: true,
                                            operation_id: "ui:project-create".to_owned(),
                                        },
                                        window,
                                        cx,
                                    );
                                    close_form.update(cx, |open, cx| {
                                        *open = false;
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            IconButton::new("cancel-project-create", IconName::X)
                                .variant(ButtonVariant::Ghost)
                                .size(ControlSize::Sm)
                                .tooltip("取消添加项目")
                                .on_click(move |_, _, cx| {
                                    cancel_form.update(cx, |open, cx| {
                                        *open = false;
                                        cx.notify();
                                    });
                                }),
                        ),
                )
                .into_any_element()
        } else {
            let open_form = add_project_open.clone();
            div()
                .child(
                    IconButton::new("show-project-create", IconName::Plus)
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .tooltip("添加项目")
                        .on_click(move |_, _, cx| {
                            open_form.update(cx, |open, cx| {
                                *open = true;
                                cx.notify();
                            });
                        }),
                )
                .into_any_element()
        };
        let mut project_rows = Vec::new();
        if let Some(page) = workspace.as_ref() {
            for (project_index, project) in page.projects.iter().enumerate() {
                project_rows.push(self.project_row(
                    project,
                    page,
                    project_index,
                    options,
                    window,
                    cx,
                ));
            }
        }
        let navigation_buttons = div()
            .flex()
            .items_center()
            .gap_1()
            .child(navigation_history_button(
                "native-sidebar-navigation-back",
                IconName::ArrowLeft,
                "后退",
                navigation_back_enabled,
                navigation_back.unwrap_or_else(|| Rc::new(|_, _| {})),
                cx,
            ))
            .child(navigation_history_button(
                "native-sidebar-navigation-forward",
                IconName::ArrowRight,
                "前进",
                navigation_forward_enabled,
                navigation_forward.unwrap_or_else(|| Rc::new(|_, _| {})),
                cx,
            ));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .bg(shell.sidebar)
            .child(
                div()
                    .flex_none()
                    .relative()
                    .h(px(48.0))
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .window_control_area(WindowControlArea::Drag),
                    )
                    .child(
                        div()
                            .relative()
                            .h_full()
                            .flex()
                            .items_center()
                            // 固定 ZCode Windows 标题栏为 ml-px + pl-3，即控件行从 x=13 开始。
                            .gap_1()
                            .pl(px(13.0))
                            .child(
                                div()
                                    .size(px(28.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    // 来源 Windows Logo 为 left=17、20px；使用现有产品图标。
                                    .child(img("native/brand/icon.png").size(px(20.0))),
                            )
                            .child(navigation_buttons),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_2()
                    .child(navigation_row(
                        "new-chat",
                        "新建任务",
                        Icon::from_path("native/icons/message-circle-plus.svg"),
                        Some(new_chat_shortcut.as_str()),
                        Rc::new(move |window, cx| {
                            if let Some(root_path) = root_path.clone() {
                                new_action(
                                    NativeUiAction::CreateSession {
                                        project_root: root_path,
                                        operation_id: "ui:new-chat".to_owned(),
                                    },
                                    window,
                                    cx,
                                )
                            }
                        }),
                        cx,
                    ))
                    .when_some(search, |nav, handler| {
                        nav.child(navigation_row(
                            "native-search",
                            "搜索",
                            Icon::new(IconName::Search),
                            Some("Ctrl+K"),
                            handler,
                            cx,
                        ))
                    })
                    .when_some(settings.clone(), |nav, handler| {
                        let automations = handler.clone();
                        nav.child(navigation_row(
                            "native-automations",
                            "自动化",
                            Icon::from_path("native/icons/calendar-clock.svg"),
                            None,
                            Rc::new(move |window, cx| {
                                automations(SettingsPage::Automations, window, cx)
                            }),
                            cx,
                        ))
                        .child(navigation_row(
                            "native-plugins",
                            "插件市场",
                            Icon::from_path("native/icons/blocks.svg"),
                            None,
                            Rc::new(move |window, cx| handler(SettingsPage::Resources, window, cx)),
                            cx,
                        ))
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .pl(px(10.0))
                    .pr_3()
                    .pb_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .h(px(28.0))
                            .p(px(2.0))
                            .rounded_full()
                            .bg(colors.surface)
                            .child(
                                div()
                                    .id("native-sidebar-group-view")
                                    .role(Role::Button)
                                    .aria_label("按分组显示")
                                    .tab_index(0)
                                    .h(px(24.0))
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .pl(px(6.0))
                                    .pr_2()
                                    .rounded_full()
                                    .text_size(tab_text_size)
                                    .text_color(if options.grouped {
                                        colors.fg
                                    } else {
                                        colors.fg_subtle
                                    })
                                    .when(options.grouped, |tab| tab.bg(shell.content))
                                    .cursor_pointer()
                                    .hover(|tab| tab.bg(colors.hover))
                                    .child(Icon::new(IconName::Hash).size(IconSize::Xs))
                                    .child("分组")
                                    .on_click(move |_, _, cx| {
                                        grouped_options.update(cx, |options, cx| {
                                            options.grouped = true;
                                            cx.notify();
                                        })
                                    }),
                            )
                            .child(
                                div()
                                    .id("native-sidebar-project-view")
                                    .role(Role::Button)
                                    .aria_label("按项目显示")
                                    .tab_index(0)
                                    .h(px(24.0))
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .pl(px(6.0))
                                    .pr_2()
                                    .rounded_full()
                                    .text_size(tab_text_size)
                                    .text_color(if options.grouped {
                                        colors.fg_subtle
                                    } else {
                                        colors.fg
                                    })
                                    .when(!options.grouped, |tab| tab.bg(shell.content))
                                    .cursor_pointer()
                                    .hover(|tab| tab.bg(colors.hover))
                                    .child(Icon::new(IconName::Folder).size(IconSize::Xs))
                                    .child("项目")
                                    .on_click(move |_, _, cx| {
                                        project_options.update(cx, |options, cx| {
                                            options.grouped = false;
                                            cx.notify();
                                        })
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                IconButton::new(
                                    "native-sidebar-collapse-all",
                                    if options.collapsed {
                                        IconName::Maximize2
                                    } else {
                                        IconName::Minimize2
                                    },
                                )
                                .variant(ButtonVariant::Ghost)
                                .size(ControlSize::Sm)
                                .tooltip(if options.collapsed {
                                    "展开全部"
                                } else {
                                    "折叠全部"
                                })
                                .on_click(move |_, _, cx| {
                                    collapse_options.update(cx, |options, cx| {
                                        options.collapsed = !options.collapsed;
                                        cx.notify();
                                    })
                                }),
                            )
                            .child(
                                ContextMenu::new(
                                    "native-sidebar-sort",
                                    Menu::new()
                                        .item(MenuItem::new("最近更新").on_click(move |_, cx| {
                                            recent_options.update(cx, |options, cx| {
                                                options.alphabetical = false;
                                                cx.notify();
                                            })
                                        }))
                                        .item(MenuItem::new("按名称").on_click(move |_, cx| {
                                            alphabetical_options.update(cx, |options, cx| {
                                                options.alphabetical = true;
                                                cx.notify();
                                            })
                                        })),
                                )
                                .manual(sort_request_value)
                                .child(
                                    div()
                                        .id("native-sidebar-sort-trigger")
                                        .role(Role::Button)
                                        .aria_label("任务排序")
                                        .tab_index(0)
                                        .size(px(24.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(6.0))
                                        .border_1()
                                        .border_color(transparent_black())
                                        .focus_ring(cx)
                                        .text_color(colors.fg_subtle)
                                        .cursor_pointer()
                                        .hover(|style| style.bg(colors.hover))
                                        .active(|style| style.bg(colors.active))
                                        .tooltip(Tooltip::text("任务排序"))
                                        .child(
                                            Icon::from_path("native/icons/list-filter.svg")
                                                .size(IconSize::Sm),
                                        )
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            move |event, window, cx| {
                                                window.prevent_default();
                                                cx.stop_propagation();
                                                sort_mouse_request.update(cx, |request, cx| {
                                                    let generation =
                                                        request.map_or(1, |(generation, _)| {
                                                            generation.wrapping_add(1)
                                                        });
                                                    *request = Some((generation, event.position));
                                                    cx.notify();
                                                });
                                            },
                                        )
                                        .on_key_down(move |event, _, cx| {
                                            if matches!(
                                                event.keystroke.key.as_str(),
                                                "enter" | "space"
                                            ) {
                                                cx.stop_propagation();
                                                sort_key_request.update(cx, |request, cx| {
                                                    let generation =
                                                        request.map_or(1, |(generation, _)| {
                                                            generation.wrapping_add(1)
                                                        });
                                                    // 键盘触发使用固定来源工具栏的逻辑坐标，不依赖上次鼠标位置。
                                                    *request = Some((
                                                        generation,
                                                        gpui::point(
                                                            px(SIDEBAR_WIDTH - 60.0),
                                                            px(240.0),
                                                        ),
                                                    ));
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                ),
                            )
                            .child(
                                IconButton::new(
                                    "native-sidebar-archived",
                                    if options.archived {
                                        IconName::X
                                    } else {
                                        IconName::Archive
                                    },
                                )
                                .variant(ButtonVariant::Ghost)
                                .size(ControlSize::Sm)
                                .tooltip(if options.archived {
                                    "关闭归档"
                                } else {
                                    "归档任务"
                                })
                                .on_click(move |_, window, cx| {
                                    let archived = !archived_options.read(cx).archived;
                                    archived_options.update(cx, |options, cx| {
                                        options.archived = archived;
                                        cx.notify();
                                    });
                                    if let Some(root_path) = archive_root.clone() {
                                        archive_action(
                                            NativeUiAction::LoadWorkspace {
                                                root_path,
                                                cursor: None,
                                                include_archived: archived,
                                            },
                                            window,
                                            cx,
                                        );
                                    }
                                }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .pt_1()
                    .pb_1()
                    .text_size(ui_text_size(cx.theme(), UiTextSize::Base))
                    .text_color(colors.fg_subtle)
                    .child(div().flex_1().font_weight(gpui::FontWeight::MEDIUM).child(
                        if options.archived {
                            "归档"
                        } else if options.grouped {
                            "分组"
                        } else {
                            "项目"
                        },
                    ))
                    .child(add_project_panel),
            )
            .child(
                div()
                    .id("native-sidebar-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(project_rows),
            )
            .when_some(load_more, |sidebar, cursor| {
                let root_path = self.root_path.clone();
                let action = self.on_action.clone();
                sidebar.child(
                    Button::new("load-more-sessions", "加载更多")
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, window, cx| {
                            if let Some(root_path) = root_path.clone() {
                                action(
                                    NativeUiAction::LoadWorkspace {
                                        root_path,
                                        cursor: Some(cursor.clone()),
                                        include_archived: options.archived,
                                    },
                                    window,
                                    cx,
                                )
                            }
                        }),
                )
            })
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .pt_2()
                    .pb_4()
                    .when_some(settings, |footer, handler| {
                        footer.child(div().flex_1().min_w_0().child(navigation_row(
                            "native-sidebar-settings",
                            "设置",
                            Icon::new(IconName::Settings),
                            None,
                            Rc::new(move |window, cx| handler(SettingsPage::General, window, cx)),
                            cx,
                        )))
                    })
                    .when_some(workbench, |footer, handler| {
                        footer.child(
                            IconButton::new("open-native-workbench", IconName::FileText)
                                .variant(ButtonVariant::Ghost)
                                .size(ControlSize::Sm)
                                .tooltip("工作台")
                                .on_click(move |_, window, cx| handler(window, cx)),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_picker_keeps_protocol_value_separate_from_label() {
        let choices = group_choices(&[GroupFact {
            group_id: "backend".to_owned(),
            project_key: "project".to_owned(),
            root_path: "project".to_owned(),
            title: "后端".to_owned(),
            session_ids: vec!["session".to_owned()],
            revision: 4,
        }]);

        assert_eq!(group_choices_to_id(&choices, "后端"), "backend");
        assert_eq!(group_choices_to_id(&choices, "未分组"), "");
        assert_eq!(group_choices_to_id(&choices, "frontend"), "frontend");
    }

    #[test]
    fn grouping_operation_id_is_stable_and_bounded() {
        let session_id = "s".repeat(1024);
        let group_id = "g".repeat(1024);
        let first = grouping_operation_id(&session_id, &group_id, 7);
        let second = grouping_operation_id(&session_id, &group_id, 7);

        assert_eq!(first, second);
        assert!(first.len() < 256);
        assert_ne!(first, grouping_operation_id(&session_id, &group_id, 8));
    }
}
