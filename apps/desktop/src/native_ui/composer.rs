//! 会话输入区：真实 TextInput、@ 提及、文件附件和会话配置动作。

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    chat::DragDropOverlay,
    forms::{Input, TextInput},
    menus::{DropdownMenu, Menu, MenuItem, OverflowMenu},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Palette, Radius},
};
use gpui::{
    AnyElement, App, Corners, Entity, Focusable, InteractiveElement, IntoElement, MouseButton,
    ParentElement, PathPromptOptions, RenderOnce, Styled, Window, canvas, div, prelude::*, px,
};

use super::model::{
    AttachmentFact, ConversationFact, DraftFact, FollowupMode, ModelCatalog, ModelSelection,
    NativeUiAction, PermissionMode, PlanMode, ProjectFact, QueuedInputFact, QueuedInputState,
    SessionFact, UsageFact,
};
use super::navigation::normalize_project_root;
use super::settings::{NativeKeybindingAction, NativeKeybindingsState, SettingsPage};
use super::style::{UiTextSize, composer_shadow, composer_surface_background, ui_text_size};

type ActionHandler = Rc<dyn Fn(NativeUiAction, &mut Window, &mut App)>;
type SettingsHandler = Rc<dyn Fn(SettingsPage, &mut Window, &mut App)>;
type DraftActionHandler = Rc<dyn Fn(DraftComposerAction, &mut Window, &mut App)>;

/// CSS 负 spread 同时收缩阴影轮廓和圆角；GPUI 默认只收缩矩形。
/// 在绘制阶段按每层 spread 调整圆角，保留输入壳的 16px 圆角和布局尺寸。
fn composer_drop_shadow(colors: &Palette) -> AnyElement {
    let shadows = composer_shadow(colors);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            for shadow in &shadows {
                let radius = (px(16.0) + shadow.spread_radius).max(px(0.0));
                window.paint_drop_shadows(
                    bounds,
                    Corners::all(radius),
                    std::slice::from_ref(shadow),
                );
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

pub(crate) const MAX_DRAFT_ATTACHMENT_PATHS: usize = 32;
pub(crate) const MAX_DRAFT_ATTACHMENT_PATH_BYTES: usize = 4096;

/// 未创建会话时只保留用户的配置意图；Host 事实仍要等 Session 创建后确认。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DraftComposerOptions {
    pub model: Option<ModelSelection>,
    pub effort: Option<String>,
    pub permission: PermissionMode,
    pub plan: PlanMode,
    /// 这里保存用户选择的路径文本，不伪造 Host 生成的 AttachmentFact。
    pub attachment_paths: Vec<String>,
}

/// Draft Composer 的本地交互，不进入 NativeHost 协议。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftComposerAction {
    Submit,
    SelectModel(ModelSelection),
    SelectEffort(String),
    SelectPermission(PermissionMode),
    SelectPlan(PlanMode),
    AddAttachmentPaths(Vec<PathBuf>),
    RemoveAttachmentPath(String),
}

fn permission_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Build => "变更前确认",
        PermissionMode::Edit => "自动编辑",
        PermissionMode::Plan => "计划模式",
        PermissionMode::Yolo => "完全访问",
    }
}

const COMPOSER_PLACEHOLDER: &str = "向 KeenCode 提问，使用 @ 添加上下文，使用 / 选择命令或能力";
// 与 ZCode 空态 Composer 保持稳定的上下文栏和编辑区边界。
const COMPOSER_PROJECT_HEADER_HEIGHT_PX: f32 = 40.0;
const COMPOSER_INPUT_MIN_HEIGHT_PX: f32 = 40.0;
const COMPOSER_INPUT_MAX_HEIGHT_PX: f32 = 160.0;
// 来源 icon-md 固定为 28px；Composer 工具栏尺寸独立于 Ely 的 density。
const COMPOSER_TOOLBAR_ICON_SIDE_PX: f32 = 28.0;
// 来源 Composer dropdown 使用固定 h-7；Compact density 下不能依赖 Ely 的 Md 高度。
const COMPOSER_TOOLBAR_DROPDOWN_HEIGHT_PX: f32 = 28.0;

fn ensure_composer_placeholder(field: &Entity<TextInput>, cx: &mut App) {
    if field.read(cx).placeholder_text().as_str() != COMPOSER_PLACEHOLDER {
        field.update(cx, |field, cx| {
            field.set_placeholder(COMPOSER_PLACEHOLDER, cx);
        });
    }
}

fn project_label(project_root: &str) -> String {
    Path::new(project_root)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("选择项目")
        .to_owned()
}

fn project_header(label: String, id: &'static str, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let text_size = ui_text_size(theme, UiTextSize::Base);
    div()
        .flex()
        .items_center()
        .gap_1()
        .min_w_0()
        .h(px(COMPOSER_PROJECT_HEADER_HEIGHT_PX))
        .flex_none()
        .px_3()
        .text_size(text_size)
        .text_color(theme.colors.fg)
        .child(Icon::new(IconName::FolderOpen).size(IconSize::Sm))
        .child(
            div()
                .id(id)
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(label),
        )
        .into_any_element()
}

fn project_display_name(project: &ProjectFact) -> String {
    if project.display_name.trim().is_empty() {
        project_label(&project.root_path)
    } else {
        project.display_name.clone()
    }
}

fn draft_project_header(
    project_root: &str,
    projects: &[ProjectFact],
    on_action: &ActionHandler,
    cx: &mut App,
) -> AnyElement {
    if projects.is_empty() {
        return project_header(project_label(project_root), "draft-project-header", cx);
    }

    // 启动根路径可带 Windows 扩展前缀和反斜杠，投影使用正斜杠；
    // 与导航共用目录规范化，才能正确显示已保存的项目名称和菜单选中态。
    let current_root = normalize_project_root(project_root);
    let menu = projects.iter().fold(Menu::new(), |menu, project| {
        let root_path = project.root_path.clone();
        let selected = normalize_project_root(&root_path) == current_root;
        let action = on_action.clone();
        menu.item(
            MenuItem::radio(project_display_name(project), selected)
                .icon(IconName::FolderOpen)
                .on_click(move |window, cx| {
                    action(
                        NativeUiAction::OpenProject {
                            project_root: root_path.clone(),
                        },
                        window,
                        cx,
                    )
                }),
        )
    });

    let label = projects
        .iter()
        .find(|project| normalize_project_root(&project.root_path) == current_root)
        .map(project_display_name)
        .unwrap_or_else(|| project_label(project_root));
    div()
        .flex()
        .items_center()
        .h(px(COMPOSER_PROJECT_HEADER_HEIGHT_PX))
        .flex_none()
        .px_3()
        .child(
            DropdownMenu::new("draft-project-header", label, menu)
                .variant(ButtonVariant::Ghost)
                .icon(IconName::FolderOpen),
        )
        .into_any_element()
}

/// 无会话时的输入态。文本只保留在 GPUI 的 TextInput 中，显式发送后才创建 Journal
/// Session；因此首屏不会因为渲染空态而产生临时会话或伪造事实快照。
#[derive(IntoElement)]
pub(crate) struct DraftComposerView {
    project_root: String,
    projects: Vec<ProjectFact>,
    catalog: Arc<Vec<ModelCatalog>>,
    options: DraftComposerOptions,
    field: Entity<TextInput>,
    keybindings: NativeKeybindingsState,
    on_action: ActionHandler,
    on_draft_action: DraftActionHandler,
    on_settings: SettingsHandler,
}

impl DraftComposerView {
    #[expect(
        clippy::too_many_arguments,
        reason = "草稿上下文与三类真实动作回调在窗口创建时独立注入"
    )]
    pub(crate) fn new(
        project_root: String,
        catalog: Arc<Vec<ModelCatalog>>,
        options: DraftComposerOptions,
        field: &Entity<TextInput>,
        keybindings: NativeKeybindingsState,
        on_action: impl Fn(NativeUiAction, &mut Window, &mut App) + 'static,
        on_draft_action: impl Fn(DraftComposerAction, &mut Window, &mut App) + 'static,
        on_settings: impl Fn(SettingsPage, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            project_root,
            projects: Vec::new(),
            catalog,
            options,
            field: field.clone(),
            keybindings,
            on_action: Rc::new(on_action),
            on_draft_action: Rc::new(on_draft_action),
            on_settings: Rc::new(on_settings),
        }
    }

    /// 由工作区投影提供可选项目；空列表时保留只读项目标题，避免伪造选择菜单。
    pub fn with_projects(mut self, projects: Vec<ProjectFact>) -> Self {
        self.projects = projects;
        self
    }

    fn model_options(&self) -> Vec<(ModelSelection, String)> {
        self.catalog
            .iter()
            .flat_map(|provider| {
                provider.models.iter().map(|model| {
                    (
                        ModelSelection {
                            provider_id: provider.provider_id.clone(),
                            model: model.model.clone(),
                        },
                        model.label.clone(),
                    )
                })
            })
            .collect()
    }

    fn effort_options(&self) -> Vec<String> {
        let options = self
            .options
            .model
            .as_ref()
            .and_then(|selected| {
                self.catalog
                    .iter()
                    .find(|provider| provider.provider_id == selected.provider_id)
                    .and_then(|provider| {
                        provider
                            .models
                            .iter()
                            .find(|model| model.model == selected.model)
                    })
            })
            .map(|model| model.reasoning_efforts.clone())
            .unwrap_or_default();
        if options.is_empty() {
            vec!["none".to_owned()]
        } else {
            options
        }
    }

    /// 来源仅在当前模型声明了推理层级时显示思考控件；空值不伪造为可选项。
    fn has_thought_option(&self) -> bool {
        self.options
            .model
            .as_ref()
            .and_then(|selected| {
                self.catalog
                    .iter()
                    .find(|provider| provider.provider_id == selected.provider_id)
                    .and_then(|provider| {
                        provider
                            .models
                            .iter()
                            .find(|model| model.model == selected.model)
                    })
            })
            .is_some_and(|model| !model.reasoning_efforts.is_empty())
    }

    fn model_label(&self) -> String {
        let Some(selection) = self.options.model.as_ref() else {
            return "选择模型".to_owned();
        };
        self.catalog
            .iter()
            .find(|provider| provider.provider_id == selection.provider_id)
            .and_then(|provider| {
                provider
                    .models
                    .iter()
                    .find(|model| model.model == selection.model)
                    .map(|model| model.label.clone())
            })
            .unwrap_or_else(|| selection.model.clone())
    }
}

impl RenderOnce for DraftComposerView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        let base_text_size = ui_text_size(cx.theme(), UiTextSize::Base);
        let radius = cx.theme().radius(Radius::Xl);
        let attachment_radius = cx.theme().radius(Radius::Sm);
        let attachment_text_size = ui_text_size(cx.theme(), UiTextSize::Xs);
        ensure_composer_placeholder(&self.field, cx);
        let model_options = self.model_options();
        let has_models = !model_options.is_empty();
        let has_thought_option = self.has_thought_option();
        let selected_model = self.options.model.clone();
        let model_label = self.model_label();
        let options = self.options.clone();
        let on_action = self.on_action.clone();
        let on_draft_action = self.on_draft_action.clone();
        let send_field = self.field.clone();
        let on_draft_action_for_send = on_draft_action.clone();
        let send = move |window: &mut Window, cx: &mut App| {
            if send_field.read(cx).text().trim().is_empty() {
                return;
            }
            on_draft_action_for_send(DraftComposerAction::Submit, window, cx);
        };
        let send_now = Rc::new(send);
        let click_field = self.field.clone();
        let input = div()
            .id("draft-composer-input-scroll")
            .key_context("NativeComposer")
            // 最小编辑高度包含控件正文之外的空白；点击整片编辑区都应获得输入焦点。
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.focus(&click_field.read(cx).focus_handle(cx), cx);
            })
            // 发送键由 NativeUi 根拦截器统一处理，避免同一 keydown 重复提交。
            .min_h(px(COMPOSER_INPUT_MIN_HEIGHT_PX))
            .max_h(px(COMPOSER_INPUT_MAX_HEIGHT_PX))
            .overflow_y_scroll()
            // TextInput 的 placeholder 比来源低约 1 个逻辑像素；相对位移不改变编辑区高度。
            .relative()
            .top(px(-1.0))
            .text_size(base_text_size)
            // 来源编辑器为 leading-5；两行最低高度仍由 min-h-10 的 40px 约束。
            .line_height(px(20.0))
            .child(self.field.clone());
        let button_send = send_now.clone();
        let send_disabled = self.field.read(cx).text().trim().is_empty();
        let send_button = IconButton::new("draft-send", IconName::ArrowUp)
            .variant(ButtonVariant::Primary)
            .size(ControlSize::Md)
            .side_length(px(COMPOSER_TOOLBAR_ICON_SIDE_PX))
            .background_inset(px(1.0))
            .tooltip(format!(
                "发送（{}）",
                self.keybindings
                    .display(NativeKeybindingAction::ComposerSubmit)
            ))
            .disabled(send_disabled)
            .disabled_opacity(0.5)
            .on_click(move |_, window, cx| button_send(window, cx));

        // 模型触发器对齐来源 Button 的右内边距、内容间距和 Chevron 尺寸；Permission 保持标准密度。
        let model_menu = {
            let action = on_draft_action.clone();
            let menu = model_options
                .into_iter()
                .fold(Menu::new(), |menu, (selection, label)| {
                    let selected = selected_model.as_ref() == Some(&selection);
                    let action = action.clone();
                    menu.item(
                        MenuItem::radio(label, selected).on_click(move |window, cx| {
                            action(
                                DraftComposerAction::SelectModel(selection.clone()),
                                window,
                                cx,
                            )
                        }),
                    )
                });
            DropdownMenu::new("draft-composer-model", model_label, menu)
                .variant(ButtonVariant::Ghost)
                .trigger_text_size(base_text_size)
                .trigger_horizontal_padding(px(8.0))
                .trigger_right_padding(px(6.0))
                .trigger_content_gap(px(4.0))
                .trigger_height(px(COMPOSER_TOOLBAR_DROPDOWN_HEIGHT_PX))
                .trigger_icon_size(IconSize::Sm)
                .into_any_element()
        };
        let model_tool = if has_models {
            model_menu
        } else {
            let on_settings = self.on_settings.clone();
            let menu = Menu::new().item(
                MenuItem::new("管理模型")
                    .on_click(move |window, cx| on_settings(SettingsPage::Providers, window, cx)),
            );
            DropdownMenu::new("draft-model-settings", "管理模型", menu)
                .variant(ButtonVariant::Ghost)
                .trigger_text_size(base_text_size)
                .trigger_horizontal_padding(px(8.0))
                .trigger_right_padding(px(6.0))
                .trigger_content_gap(px(4.0))
                .trigger_height(px(COMPOSER_TOOLBAR_DROPDOWN_HEIGHT_PX))
                .trigger_icon_size(IconSize::Sm)
                .into_any_element()
        };

        let permission_menu = {
            let current = options.permission;
            let action = on_draft_action.clone();
            let modes = [
                (
                    PermissionMode::Build,
                    permission_label(PermissionMode::Build),
                ),
                (PermissionMode::Edit, permission_label(PermissionMode::Edit)),
                (PermissionMode::Plan, permission_label(PermissionMode::Plan)),
                (PermissionMode::Yolo, permission_label(PermissionMode::Yolo)),
            ];
            let menu = modes.into_iter().fold(Menu::new(), |menu, (mode, label)| {
                let action = action.clone();
                menu.item(
                    MenuItem::radio(label, current == mode)
                        .icon(IconName::Shield)
                        .on_click(move |window, cx| {
                            action(DraftComposerAction::SelectPermission(mode), window, cx)
                        }),
                )
            });
            DropdownMenu::new("draft-composer-permission", permission_label(current), menu)
                .variant(ButtonVariant::Ghost)
                .icon(IconName::Hand)
                .trigger_text_size(base_text_size)
                .trigger_horizontal_padding(px(8.0))
                .trigger_height(px(COMPOSER_TOOLBAR_DROPDOWN_HEIGHT_PX))
                .trigger_icon_size(IconSize::Md)
        };
        let effort_items = {
            let current = options.effort.as_deref().unwrap_or("none");
            let action = on_draft_action.clone();
            self.effort_options()
                .into_iter()
                .map(|effort| {
                    let action = action.clone();
                    let selected = effort == current;
                    MenuItem::radio(format!("思考 {effort}"), selected).on_click(
                        move |window, cx| {
                            action(
                                DraftComposerAction::SelectEffort(effort.clone()),
                                window,
                                cx,
                            )
                        },
                    )
                })
                .collect::<Vec<_>>()
        };
        let plan_items = {
            let current = options.plan;
            let action = on_draft_action.clone();
            [(PlanMode::Off, "Plan 关"), (PlanMode::ReadOnly, "Plan 开")]
                .into_iter()
                .map(|(mode, label)| {
                    let action = action.clone();
                    MenuItem::radio(label, current == mode).on_click(move |window, cx| {
                        action(DraftComposerAction::SelectPlan(mode), window, cx)
                    })
                })
                .collect::<Vec<_>>()
        };
        let more_menu = if has_thought_option {
            Menu::new()
                .group("思考强度", effort_items)
                .group("计划模式", plan_items)
        } else {
            Menu::new().group("计划模式", plan_items)
        };
        let more_button = OverflowMenu::new("draft-composer-more", more_menu)
            .icon(IconName::SlidersHorizontal)
            .tooltip("更多会话设置");

        let attachment_action = on_draft_action.clone();
        let attachment_action_for_button = attachment_action.clone();
        // 文件只在用户提交后交给 Host 解析；Draft 阶段保留路径意图，不构造附件事实。
        let attachment_button = IconButton::new("draft-attach-file", IconName::Plus)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Md)
            .side_length(px(COMPOSER_TOOLBAR_ICON_SIDE_PX))
            .tooltip("添加上下文文件")
            .on_click(move |_, window, cx| {
                let chosen = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: true,
                    prompt: Some("添加上下文文件".into()),
                });
                let action = attachment_action_for_button.clone();
                window
                    .spawn(cx, async move |cx| match chosen.await {
                        Ok(Ok(Some(paths))) if !paths.is_empty() => {
                            let _ = cx.update(|window, cx| {
                                action(DraftComposerAction::AddAttachmentPaths(paths), window, cx)
                            });
                        }
                        Ok(Err(error)) => tracing::warn!(%error, "原生附件选择失败"),
                        _ => {}
                    })
                    .detach();
            });

        let drop_action = attachment_action.clone();
        let input_shell = div()
            .id("draft-composer")
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .rounded(px(16.0))
            .child(composer_drop_shadow(&colors))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .rounded(px(16.0))
                    .bg(composer_surface_background(&colors))
                    .overflow_hidden()
                    .child(draft_project_header(
                        &self.project_root,
                        &self.projects,
                        &on_action,
                        cx,
                    ))
                    .child(
                        div()
                            .id("draft-composer-input-frame")
                            .track_focus(&self.field.read(cx).focus_handle(cx))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_3()
                            .rounded(px(16.0))
                            .border_1()
                            .border_color(colors.border)
                            // 来源输入壳默认使用 border，悬停与正文焦点才加强边界。
                            .hover(|view| view.border_color(colors.border_strong))
                            .focus(|view| view.border_color(colors.border_strong))
                            .bg(colors.sunken)
                            .text_size(base_text_size)
                            .child(input)
                            .child(
                                div()
                                    .flex()
                                    .items_end()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex()
                                            .min_w_0()
                                            .flex_1()
                                            .items_center()
                                            .gap_1()
                                            .child(attachment_button)
                                            .child(permission_menu),
                                    )
                                    // 模型与发送按钮沿用来源提交控制簇的 4px 外间距。
                                    .child(
                                        div()
                                            .flex()
                                            .flex_shrink_0()
                                            .items_center()
                                            .justify_end()
                                            .gap_1()
                                            .when(has_models, |toolbar| toolbar.child(more_button))
                                            .child(model_tool)
                                            .child(send_button),
                                    ),
                            ),
                    ),
            )
            .child(DragDropOverlay::new(move |paths, window, cx| {
                drop_action(DraftComposerAction::AddAttachmentPaths(paths), window, cx);
            }));

        let attachment_paths = options.attachment_paths.clone();
        let has_attachments = !attachment_paths.is_empty();
        let attachment_chips = attachment_paths.into_iter().map(|path| {
            let label = Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or(path.as_str())
                .to_owned();
            let action = attachment_action.clone();
            let remove_path = path.clone();
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .rounded(attachment_radius)
                .bg(colors.sunken)
                .text_size(attachment_text_size)
                .child(label)
                .child(
                    Button::new(format!("draft-remove-attachment:{path}"), "移除")
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, window, cx| {
                            action(
                                DraftComposerAction::RemoveAttachmentPath(remove_path.clone()),
                                window,
                                cx,
                            )
                        }),
                )
                .into_any_element()
        });

        div()
            .flex()
            .flex_col()
            .gap_6()
            .w_full()
            .when(!has_models, |body| {
                let on_settings = self.on_settings.clone();
                body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .w_full()
                        .px_3()
                        .py_2()
                        .rounded(radius)
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.surface)
                        .text_size(base_text_size)
                        .text_color(colors.fg)
                        .child(Icon::new(IconName::Info).size(IconSize::Sm))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child("当前没有可用模型。请配置自定义模型。"),
                        )
                        .child(
                            Button::new("draft-model-banner-settings", "配置")
                                .variant(ButtonVariant::Outline)
                                // Compact 下 Md 为 24px，对应来源 h-6；不能按两库的 size 名称直译。
                                .size(ControlSize::Md)
                                .icon(IconName::Settings)
                                .on_click(move |_, window, cx| {
                                    on_settings(SettingsPage::Providers, window, cx)
                                }),
                        ),
                )
            })
            .child(input_shell)
            .when(has_attachments, |body| {
                body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(colors.fg_subtle)
                        .child(div().flex().flex_wrap().gap_1().children(attachment_chips)),
                )
            })
    }
}

/// Composer 只读取当前事实来决定可用控件；状态变更经 `NativeUiAction` 交给宿主。
#[derive(IntoElement)]
pub struct ComposerView {
    session_id: String,
    session: SessionFact,
    usage: Option<UsageFact>,
    input_queue: Vec<QueuedInputFact>,
    draft: DraftFact,
    field: Entity<TextInput>,
    keybindings: NativeKeybindingsState,
    catalog: Arc<Vec<ModelCatalog>>,
    on_action: ActionHandler,
}

/// 发送回调只保留发送所需的事实，避免为每个按钮复制整个 ComposerView。
struct ComposerSendContext {
    session_id: String,
    field: Entity<TextInput>,
    attachments: Vec<AttachmentFact>,
    model: Option<ModelSelection>,
    effort: Option<String>,
    permission: PermissionMode,
    plan: PlanMode,
    on_action: ActionHandler,
}

impl ComposerSendContext {
    fn send(&self, window: &mut Window, cx: &mut App) {
        let text = self.field.read(cx).text().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let session_id = self.session_id.clone();
        (self.on_action)(
            NativeUiAction::Send {
                session_id: session_id.clone(),
                text,
                attachments: self.attachments.clone(),
                model: self.model.clone(),
                effort: self.effort.clone(),
                permission: self.permission,
                plan: self.plan,
                draft_edit_generation: 0,
                operation_id: format!("ui:send:{session_id}"),
            },
            window,
            cx,
        );
    }
}

/// 文件选择和拖拽共享同一份轻量会话上下文，不复制队列、目录或用量事实。
struct ComposerAttachmentContext {
    session_id: String,
    on_action: ActionHandler,
}

impl ComposerAttachmentContext {
    fn attach(&self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut App) {
        for path in paths {
            let path = path.to_string_lossy().into_owned();
            (self.on_action)(
                NativeUiAction::AddAttachment {
                    session_id: self.session_id.clone(),
                    path,
                    operation_id: format!("ui:attachment:{}", self.session_id),
                },
                window,
                cx,
            );
        }
    }
}

impl ComposerView {
    pub fn new(
        session_id: String,
        conversation: &ConversationFact,
        field: &Entity<TextInput>,
        catalog: Arc<Vec<ModelCatalog>>,
        keybindings: NativeKeybindingsState,
        on_action: impl Fn(NativeUiAction, &mut Window, &mut App) + 'static,
    ) -> Self {
        let session = conversation.session.clone();
        let input_queue = conversation.input_queue.clone();
        let draft = conversation.draft.clone();
        let usage = session.usage.clone();
        Self {
            session_id,
            session,
            usage,
            input_queue,
            draft,
            field: field.clone(),
            keybindings,
            catalog,
            on_action: Rc::new(on_action),
        }
    }

    fn selected_model(&self) -> Option<ModelSelection> {
        self.session.model.clone()
    }

    fn model_options(&self) -> Vec<(ModelSelection, String)> {
        self.catalog
            .iter()
            .flat_map(|provider| {
                provider.models.iter().map(|model| {
                    (
                        ModelSelection {
                            provider_id: provider.provider_id.clone(),
                            model: model.model.clone(),
                        },
                        model.label.clone(),
                    )
                })
            })
            .collect()
    }

    fn effort_options(&self) -> Vec<String> {
        let options = self
            .selected_model()
            .and_then(|selected| {
                self.catalog
                    .iter()
                    .find(|provider| provider.provider_id == selected.provider_id)
                    .and_then(|provider| {
                        provider
                            .models
                            .iter()
                            .find(|model| model.model == selected.model)
                    })
            })
            .map(|model| model.reasoning_efforts.clone())
            .unwrap_or_default();
        if options.is_empty() {
            vec!["none".to_owned()]
        } else {
            options
        }
    }

    /// 来源仅在当前模型声明了推理层级时显示思考控件；空值不伪造为可选项。
    fn has_thought_option(&self) -> bool {
        self.selected_model()
            .as_ref()
            .and_then(|selected| {
                self.catalog
                    .iter()
                    .find(|provider| provider.provider_id == selected.provider_id)
                    .and_then(|provider| {
                        provider
                            .models
                            .iter()
                            .find(|model| model.model == selected.model)
                    })
            })
            .is_some_and(|model| !model.reasoning_efforts.is_empty())
    }

    fn model_label(&self) -> String {
        let Some(selection) = self.selected_model() else {
            return "选择模型".to_owned();
        };
        self.catalog
            .iter()
            .find(|provider| provider.provider_id == selection.provider_id)
            .and_then(|provider| {
                provider
                    .models
                    .iter()
                    .find(|model| model.model == selection.model)
                    .map(|model| model.label.clone())
            })
            .unwrap_or(selection.model)
    }

    fn queue_panel(&self, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        if self.input_queue.is_empty() {
            return None;
        }
        let (colors, row_radius, panel_radius, small_text_size) = {
            let theme = cx.theme();
            (
                theme.colors.clone(),
                theme.radius(Radius::Sm),
                theme.radius(Radius::Md),
                ui_text_size(theme, UiTextSize::Xs),
            )
        };
        let queue = self.input_queue.clone();
        // 所有行共享同一份基线顺序；只有真正点击重排时才复制并交换两个 ID。
        let queue_ids = Rc::new(
            queue
                .iter()
                .map(|item| item.queue_item_id.clone())
                .collect::<Vec<_>>(),
        );
        let session_id = self.session_id.clone();
        let queue_fields = Rc::new(RefCell::new(
            Vec::<(String, String, Entity<TextInput>)>::new(),
        ));
        let mut rows = Vec::with_capacity(queue.len());
        for (index, item) in queue.into_iter().enumerate() {
            let item_id = item.queue_item_id.clone();
            let initial_text = item.text.clone();
            let text_complete = item.text_complete;
            let field = window.use_keyed_state(
                format!(
                    "native-queued-input:{}:{}:{}",
                    item.queue_item_id,
                    if text_complete { "full" } else { "preview" },
                    item.text_fingerprint
                ),
                cx,
                move |window, cx| {
                    let mut field = TextInput::new(window, cx).placeholder("排队输入");
                    field.set_text(initial_text, cx);
                    field
                },
            );
            queue_fields
                .borrow_mut()
                .push((item_id.clone(), item.text.clone(), field.clone()));
            let locked = !matches!(item.state, QueuedInputState::Waiting);
            let field_disabled = locked || !text_complete;
            if field.read(cx).is_disabled() != field_disabled {
                field.update(cx, |field, cx| field.set_disabled(field_disabled, cx));
            }
            let state_label = match item.state {
                QueuedInputState::Waiting => "等待发送",
                QueuedInputState::Reserved => "已锁定",
                QueuedInputState::Dispatching => "发送中",
            };
            let can_move_up = !locked
                && index > 0
                && matches!(self.input_queue[index - 1].state, QueuedInputState::Waiting);
            let can_move_down = !locked
                && index + 1 < self.input_queue.len()
                && matches!(self.input_queue[index + 1].state, QueuedInputState::Waiting);
            let on_action = self.on_action.clone();
            let edit_session = session_id.clone();
            let edit_id = item_id.clone();
            let edit_field = field.clone();
            let load_session = session_id.clone();
            let load_id = item_id.clone();
            let load_fields = Rc::clone(&queue_fields);
            let move_up = self.on_action.clone();
            let move_down = self.on_action.clone();
            let send_action = self.on_action.clone();
            let delete_action = self.on_action.clone();
            let up_item_id = item_id.clone();
            let down_item_id = item_id.clone();
            let send_item_id = item_id.clone();
            let delete_item_id = item_id.clone();
            let up_session = session_id.clone();
            let down_session = session_id.clone();
            let send_session = session_id.clone();
            let delete_session = session_id.clone();
            let up_ids = Rc::clone(&queue_ids);
            let down_ids = Rc::clone(&queue_ids);
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_1()
                    .rounded(row_radius)
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.sunken)
                    .child(Icon::new(IconName::List).size(ely_gpui_component::theme::IconSize::Xs))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&field).size(ControlSize::Sm)),
                    )
                    .child(
                        div()
                            .text_size(small_text_size)
                            .text_color(colors.fg_subtle)
                            .child(state_label),
                    )
                    .child(
                        IconButton::new(format!("queued-edit:{item_id}"), IconName::Check)
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .tooltip(if text_complete {
                                "保存排队输入"
                            } else {
                                "加载完整内容"
                            })
                            .disabled(locked)
                            .on_click(move |_, window, cx| {
                                if !text_complete {
                                    // 另一个已加载项仍有本地未保存正文时，先发出显式保存动作；
                                    // 等 Host 的 InputQueueChanged 回执到达后，用户再加载本项。
                                    let dirty_items = load_fields
                                        .borrow()
                                        .iter()
                                        .filter_map(|(dirty_id, saved_text, dirty_field)| {
                                            let text = dirty_field.read(cx).text().to_owned();
                                            (text.as_str() != saved_text.as_str())
                                                .then_some((dirty_id.clone(), text))
                                        })
                                        .collect::<Vec<_>>();
                                    if !dirty_items.is_empty() {
                                        for (dirty_id, text) in dirty_items {
                                            on_action(
                                                NativeUiAction::EditQueuedInput {
                                                    session_id: load_session.clone(),
                                                    queue_item_id: dirty_id.clone(),
                                                    text,
                                                    operation_id: format!(
                                                        "ui:queue-edit:{dirty_id}"
                                                    ),
                                                },
                                                window,
                                                cx,
                                            );
                                        }
                                        return;
                                    }
                                    on_action(
                                        NativeUiAction::LoadQueuedInput {
                                            session_id: load_session.clone(),
                                            queue_item_id: load_id.clone(),
                                        },
                                        window,
                                        cx,
                                    );
                                    return;
                                }
                                let text = edit_field.read(cx).text().trim().to_owned();
                                if text.is_empty() {
                                    return;
                                }
                                on_action(
                                    NativeUiAction::EditQueuedInput {
                                        session_id: edit_session.clone(),
                                        queue_item_id: edit_id.clone(),
                                        text,
                                        operation_id: format!("ui:queue-edit:{edit_id}"),
                                    },
                                    window,
                                    cx,
                                );
                            }),
                    )
                    .child(
                        IconButton::new(format!("queued-up:{item_id}"), IconName::ArrowUp)
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .tooltip("上移")
                            .disabled(!can_move_up)
                            .on_click(move |_, window, cx| {
                                if !can_move_up {
                                    return;
                                }
                                let mut queue_item_ids = up_ids.as_ref().clone();
                                queue_item_ids.swap(index, index - 1);
                                move_up(
                                    NativeUiAction::ReorderQueuedInput {
                                        session_id: up_session.clone(),
                                        queue_item_ids,
                                        operation_id: format!("ui:queue-up:{up_item_id}"),
                                    },
                                    window,
                                    cx,
                                )
                            }),
                    )
                    .child(
                        IconButton::new(format!("queued-down:{item_id}"), IconName::ArrowDown)
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .tooltip("下移")
                            .disabled(!can_move_down)
                            .on_click(move |_, window, cx| {
                                if !can_move_down {
                                    return;
                                }
                                let mut queue_item_ids = down_ids.as_ref().clone();
                                queue_item_ids.swap(index, index + 1);
                                move_down(
                                    NativeUiAction::ReorderQueuedInput {
                                        session_id: down_session.clone(),
                                        queue_item_ids,
                                        operation_id: format!("ui:queue-down:{down_item_id}"),
                                    },
                                    window,
                                    cx,
                                )
                            }),
                    )
                    .child(
                        IconButton::new(format!("queued-send:{item_id}"), IconName::Send)
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .tooltip("发送排队输入")
                            .disabled(locked)
                            .on_click(move |_, window, cx| {
                                send_action(
                                    NativeUiAction::SendQueuedInput {
                                        session_id: send_session.clone(),
                                        queue_item_id: send_item_id.clone(),
                                        operation_id: format!("ui:queue-send:{send_item_id}"),
                                    },
                                    window,
                                    cx,
                                )
                            }),
                    )
                    .child(
                        IconButton::new(format!("queued-delete:{item_id}"), IconName::Trash2)
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .tooltip("删除排队输入")
                            .disabled(locked)
                            .on_click(move |_, window, cx| {
                                delete_action(
                                    NativeUiAction::DeleteQueuedInput {
                                        session_id: delete_session.clone(),
                                        queue_item_id: delete_item_id.clone(),
                                        operation_id: format!("ui:queue-delete:{delete_item_id}"),
                                    },
                                    window,
                                    cx,
                                )
                            }),
                    )
                    .into_any_element(),
            );
        }
        Some(
            div()
                .id("native-composer-queued-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_1()
                .max_h(px(128.0))
                .overflow_y_scroll()
                .rounded(panel_radius)
                .bg(colors.bg)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(small_text_size)
                        .text_color(colors.fg_muted)
                        .child(
                            Icon::new(IconName::List).size(ely_gpui_component::theme::IconSize::Xs),
                        )
                        .child("排队输入"),
                )
                .children(rows)
                .into_any_element(),
        )
    }

    fn history_panel(&self, cx: &mut App) -> Option<AnyElement> {
        let entries = self.session.prompt_history.entries.clone();
        if entries.is_empty() {
            return None;
        }
        let (colors, panel_radius, small_text_size) = {
            let theme = cx.theme();
            (
                theme.colors.clone(),
                theme.radius(Radius::Md),
                ui_text_size(theme, UiTextSize::Xs),
            )
        };
        let selected = self.session.prompt_history.selected;
        let omitted = self.session.prompt_history.omitted;
        let session_id = self.session_id.clone();
        let on_action = self.on_action.clone();
        let rows = entries.into_iter().enumerate().map(|(index, entry)| {
            let action = on_action.clone();
            let session_id = session_id.clone();
            let entry_id = entry.entry_id.clone();
            Button::new(format!("prompt-history:{}", entry.entry_id), entry.text)
                .variant(if selected == Some(index) {
                    ButtonVariant::Subtle
                } else {
                    ButtonVariant::Ghost
                })
                .size(ControlSize::Sm)
                .icon(IconName::History)
                .on_click(move |_, window, cx| {
                    action(
                        NativeUiAction::SelectPromptHistory {
                            session_id: session_id.clone(),
                            entry_id: entry_id.clone(),
                        },
                        window,
                        cx,
                    )
                })
                .into_any_element()
        });
        Some(
            div()
                .id("native-composer-history-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_1()
                .max_h(px(120.0))
                .overflow_y_scroll()
                .rounded(panel_radius)
                .bg(colors.bg)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(small_text_size)
                        .text_color(colors.fg_muted)
                        .child(
                            Icon::new(IconName::History)
                                .size(ely_gpui_component::theme::IconSize::Xs),
                        )
                        .child("Prompt History"),
                )
                .when(omitted, |panel| {
                    panel.child(
                        div()
                            .text_size(small_text_size)
                            .text_color(colors.fg_subtle)
                            .child("部分较长输入未显示"),
                    )
                })
                .children(rows)
                .into_any_element(),
        )
    }
}

impl RenderOnce for ComposerView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let attachment_radius = theme.radius(Radius::Sm);
        let attachment_text_size = ui_text_size(theme, UiTextSize::Xs);
        let base_text_size = ui_text_size(theme, UiTextSize::Base);
        let shell_radius = px(16.0);
        let has_thought_option = self.has_thought_option();
        ensure_composer_placeholder(&self.field, cx);

        let busy = matches!(
            self.session.status,
            super::model::SessionStatus::Running
                | super::model::SessionStatus::Queued
                | super::model::SessionStatus::Waiting
        );

        let send_context = Rc::new(ComposerSendContext {
            session_id: self.session_id.clone(),
            field: self.field.clone(),
            attachments: self.draft.attachments.clone(),
            model: self.selected_model(),
            effort: self.session.effort.clone(),
            permission: self.session.permission,
            plan: self.session.plan,
            on_action: self.on_action.clone(),
        });
        let send = Rc::new(move |window: &mut Window, cx: &mut App| {
            send_context.send(window, cx);
        });
        let click_field = self.field.clone();
        let input = div()
            .id("native-composer-input-scroll")
            .key_context("NativeComposer")
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.focus(&click_field.read(cx).focus_handle(cx), cx);
            })
            // 发送键由 NativeUi 根拦截器统一处理，避免同一 keydown 重复提交。
            .min_h(px(COMPOSER_INPUT_MIN_HEIGHT_PX))
            .max_h(px(COMPOSER_INPUT_MAX_HEIGHT_PX))
            .overflow_y_scroll()
            // TextInput 的 placeholder 比来源低约 1 个逻辑像素；相对位移不改变编辑区高度。
            .relative()
            .top(px(-1.0))
            .text_size(base_text_size)
            .line_height(px(20.0))
            .child(self.field.clone());

        let selected_model = self.selected_model();
        // 会话模型触发器沿用来源模型控件密度；Permission 保持标准密度。
        let model_menu = {
            let session_id = self.session_id.clone();
            let on_action = self.on_action.clone();
            let menu =
                self.model_options()
                    .into_iter()
                    .fold(Menu::new(), |menu, (selection, label)| {
                        let selected = selected_model.as_ref() == Some(&selection);
                        let action = on_action.clone();
                        let action_session = session_id.clone();
                        menu.item(
                            MenuItem::radio(label, selected).on_click(move |window, cx| {
                                action(
                                    NativeUiAction::SetModel {
                                        session_id: action_session.clone(),
                                        selection: selection.clone(),
                                        operation_id: format!("ui:model:{}", action_session),
                                    },
                                    window,
                                    cx,
                                )
                            }),
                        )
                    });
            DropdownMenu::new("composer-model", self.model_label(), menu)
                .variant(ButtonVariant::Ghost)
                .trigger_text_size(base_text_size)
                .trigger_horizontal_padding(px(8.0))
                .trigger_right_padding(px(6.0))
                .trigger_content_gap(px(4.0))
                .trigger_height(px(COMPOSER_TOOLBAR_DROPDOWN_HEIGHT_PX))
                .trigger_icon_size(IconSize::Sm)
        };

        let permission_menu = {
            let session_id = self.session_id.clone();
            let on_action = self.on_action.clone();
            let current = self.session.permission;
            let modes = [
                (
                    PermissionMode::Build,
                    permission_label(PermissionMode::Build),
                ),
                (PermissionMode::Edit, permission_label(PermissionMode::Edit)),
                (PermissionMode::Plan, permission_label(PermissionMode::Plan)),
                (PermissionMode::Yolo, permission_label(PermissionMode::Yolo)),
            ];
            let menu = modes.into_iter().fold(Menu::new(), |menu, (mode, label)| {
                let action = on_action.clone();
                let action_session = session_id.clone();
                menu.item(
                    MenuItem::radio(label, current == mode)
                        .icon(IconName::Shield)
                        .on_click(move |window, cx| {
                            action(
                                NativeUiAction::SetPermission {
                                    session_id: action_session.clone(),
                                    mode,
                                    operation_id: format!("ui:permission:{}", action_session),
                                },
                                window,
                                cx,
                            )
                        }),
                )
            });
            DropdownMenu::new("composer-permission", permission_label(current), menu)
                .variant(ButtonVariant::Ghost)
                .icon(IconName::Hand)
                .trigger_text_size(base_text_size)
                .trigger_horizontal_padding(px(8.0))
                .trigger_height(px(COMPOSER_TOOLBAR_DROPDOWN_HEIGHT_PX))
                .trigger_icon_size(IconSize::Md)
        };

        let effort_items = {
            let session_id = self.session_id.clone();
            let on_action = self.on_action.clone();
            let current = self.session.effort.as_deref().unwrap_or("none");
            self.effort_options()
                .into_iter()
                .map(|effort| {
                    let action = on_action.clone();
                    let action_session = session_id.clone();
                    MenuItem::radio(format!("思考 {effort}"), effort == current).on_click(
                        move |window, cx| {
                            action(
                                NativeUiAction::SetEffort {
                                    session_id: action_session.clone(),
                                    effort: effort.clone(),
                                    operation_id: format!("ui:effort:{}", action_session),
                                },
                                window,
                                cx,
                            )
                        },
                    )
                })
                .collect::<Vec<_>>()
        };
        let plan_items = {
            let session_id = self.session_id.clone();
            let on_action = self.on_action.clone();
            let current = self.session.plan;
            [(PlanMode::Off, "Plan 关"), (PlanMode::ReadOnly, "Plan 开")]
                .into_iter()
                .map(|(mode, label)| {
                    let action = on_action.clone();
                    let action_session = session_id.clone();
                    MenuItem::radio(label, current == mode).on_click(move |window, cx| {
                        action(
                            NativeUiAction::SetPlan {
                                session_id: action_session.clone(),
                                mode,
                                operation_id: format!("ui:plan:{}", action_session),
                            },
                            window,
                            cx,
                        )
                    })
                })
                .collect::<Vec<_>>()
        };
        let vision_items = {
            let session_id = self.session_id.clone();
            let on_action = self.on_action.clone();
            let current = self.session.vision_enabled;
            [(false, "视觉关"), (true, "视觉开")]
                .into_iter()
                .map(|(enabled, label)| {
                    let action = on_action.clone();
                    let action_session = session_id.clone();
                    MenuItem::radio(label, current == enabled).on_click(move |window, cx| {
                        action(
                            NativeUiAction::SetVision {
                                session_id: action_session.clone(),
                                enabled,
                                operation_id: format!("ui:vision:{}", action_session),
                            },
                            window,
                            cx,
                        )
                    })
                })
                .collect::<Vec<_>>()
        };
        let followup_items = {
            let session_id = self.session_id.clone();
            let on_action = self.on_action.clone();
            let current = self.session.followup_mode;
            [
                (FollowupMode::Queue, "忙时排队"),
                (FollowupMode::Guide, "忙时引导"),
            ]
            .into_iter()
            .map(|(mode, label)| {
                let action = on_action.clone();
                let action_session = session_id.clone();
                MenuItem::radio(label, current == mode).on_click(move |window, cx| {
                    action(
                        NativeUiAction::SetFollowupMode {
                            session_id: action_session.clone(),
                            mode,
                            operation_id: format!("ui:followup:{}", action_session),
                        },
                        window,
                        cx,
                    )
                })
            })
            .collect::<Vec<_>>()
        };
        let more_menu = if has_thought_option {
            Menu::new()
                .group("思考强度", effort_items)
                .group("计划模式", plan_items)
        } else {
            Menu::new().group("计划模式", plan_items)
        }
        .group("视觉", vision_items)
        .group("忙时策略", followup_items);
        let more_button = OverflowMenu::new("composer-more", more_menu)
            .icon(IconName::SlidersHorizontal)
            .tooltip("更多会话设置");

        let attachment_context = Rc::new(ComposerAttachmentContext {
            session_id: self.session_id.clone(),
            on_action: self.on_action.clone(),
        });
        let attachment_context_for_button = Rc::clone(&attachment_context);
        // 来源使用“+”入口；保留 Ely 按钮键盘语义，文件选择仍由 GPUI 原生对话框处理。
        let attachment_button = IconButton::new("attach-file", IconName::Plus)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Md)
            .side_length(px(COMPOSER_TOOLBAR_ICON_SIDE_PX))
            .tooltip("添加上下文文件")
            .on_click(move |_, window, cx| {
                let chosen = cx.prompt_for_paths(PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: true,
                    prompt: Some("添加上下文文件".into()),
                });
                let on_drop = Rc::clone(&attachment_context_for_button);
                window
                    .spawn(cx, async move |cx| match chosen.await {
                        Ok(Ok(Some(paths))) if !paths.is_empty() => {
                            let _ = cx.update(|window, cx| on_drop.attach(paths, window, cx));
                        }
                        Ok(Err(error)) => tracing::warn!(%error, "原生附件选择失败"),
                        _ => {}
                    })
                    .detach();
            });
        let send_ready = !self.field.read(cx).text().trim().is_empty();
        let send_button = if busy {
            let stop_action = self.on_action.clone();
            let stop_session = self.session_id.clone();
            IconButton::new("composer-send", IconName::CircleStop)
                .variant(ButtonVariant::Secondary)
                .size(ControlSize::Md)
                .side_length(px(COMPOSER_TOOLBAR_ICON_SIDE_PX))
                .tooltip("停止")
                .on_click(move |_, window, cx| {
                    stop_action(
                        NativeUiAction::Stop {
                            session_id: stop_session.clone(),
                            operation_id: format!("ui:stop:{stop_session}"),
                        },
                        window,
                        cx,
                    )
                })
        } else {
            let send = send.clone();
            IconButton::new("composer-send", IconName::ArrowUp)
                .variant(ButtonVariant::Primary)
                .size(ControlSize::Md)
                .side_length(px(COMPOSER_TOOLBAR_ICON_SIDE_PX))
                .background_inset(px(1.0))
                .tooltip(format!(
                    "发送（{}）",
                    self.keybindings
                        .display(NativeKeybindingAction::ComposerSubmit)
                ))
                .disabled(!send_ready)
                .disabled_opacity(0.5)
                .on_click(move |_, window, cx| send(window, cx))
        };

        let drop_view = Rc::clone(&attachment_context);
        let input_shell = div()
            .id("native-composer")
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .rounded(shell_radius)
            .child(composer_drop_shadow(&colors))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .rounded(shell_radius)
                    .bg(composer_surface_background(&colors))
                    .overflow_hidden()
                    .child(project_header(
                        project_label(&self.session.project_key),
                        "native-composer-project-header",
                        cx,
                    ))
                    .child(
                        div()
                            .id("composer-input-frame")
                            .track_focus(&self.field.read(cx).focus_handle(cx))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_3()
                            .rounded(px(16.0))
                            .border_1()
                            .border_color(colors.border)
                            .hover(|view| view.border_color(colors.border_strong))
                            .focus(|view| view.border_color(colors.border_strong))
                            .bg(colors.sunken)
                            .text_size(base_text_size)
                            .child(input)
                            .child(
                                div()
                                    .flex()
                                    .items_end()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex()
                                            .min_w_0()
                                            .flex_1()
                                            .items_center()
                                            .gap_1()
                                            .child(attachment_button)
                                            .child(permission_menu),
                                    )
                                    // 模型与发送按钮沿用来源提交控制簇的 4px 外间距。
                                    .child(
                                        div()
                                            .flex()
                                            .flex_shrink_0()
                                            .items_center()
                                            .justify_end()
                                            .gap_1()
                                            .child(more_button)
                                            .child(model_menu)
                                            .child(send_button),
                                    ),
                            ),
                    ),
            )
            .child(DragDropOverlay::new(move |paths, window, cx| {
                drop_view.attach(paths, window, cx);
            }));

        let attachments = self.draft.attachments.clone();
        let has_attachments = !attachments.is_empty();
        let attachment_action = self.on_action.clone();
        let attach_session = self.session_id.clone();
        let chips = attachments.into_iter().map(|attachment| {
            let attachment_id = attachment.attachment_id.clone();
            let on_action = attachment_action.clone();
            let session_id = attach_session.clone();
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .rounded(attachment_radius)
                .bg(colors.sunken)
                .text_size(attachment_text_size)
                .child(attachment.file_name)
                .child(
                    Button::new(format!("remove-attachment:{attachment_id}"), "移除")
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, window, cx| {
                            on_action(
                                NativeUiAction::RemoveAttachment {
                                    session_id: session_id.clone(),
                                    attachment_id: attachment_id.clone(),
                                },
                                window,
                                cx,
                            )
                        }),
                )
                .into_any_element()
        });

        let usage = self.usage.as_ref();
        let has_usage = usage.is_some_and(|value| {
            value.input_tokens.is_some()
                || value.output_tokens.is_some()
                || value.reasoning_tokens.is_some()
                || value.context_window.is_some()
                || value.duration_ms.is_some()
                || value.tokens_per_second.is_some()
        });
        let usage_footer = has_usage.then(|| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_color(colors.fg_subtle)
                .text_size(attachment_text_size)
                .child(format!(
                    "输入 {}",
                    format_usage_value(usage.and_then(|value| value.input_tokens))
                ))
                .child(format!(
                    "输出 {}",
                    format_usage_value(usage.and_then(|value| value.output_tokens))
                ))
                .child(format!(
                    "推理 {}",
                    format_usage_value(usage.and_then(|value| value.reasoning_tokens))
                ))
                .child(format!(
                    "上下文窗口 {}",
                    format_usage_value(usage.and_then(|value| value.context_window))
                ))
                .child(format!(
                    "生成 {}",
                    format_duration_value(usage.and_then(|value| value.duration_ms))
                ))
                .child(format!(
                    "TPS {}",
                    format_usage_value(usage.and_then(|value| value.tokens_per_second))
                ))
                .into_any_element()
        });

        let mut body = div().flex().flex_col().gap_1().w_full();
        if let Some(queue) = self.queue_panel(window, cx) {
            body = body.child(queue);
        }
        if let Some(history) = self.history_panel(cx) {
            body = body.child(history);
        }
        body.flex()
            .flex_col()
            .gap_1()
            .w_full()
            .child(input_shell)
            .when_some(usage_footer, |composer, footer| composer.child(footer))
            .when(has_attachments, |composer| {
                composer.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(colors.fg_subtle)
                        .child(div().flex().flex_wrap().gap_1().children(chips)),
                )
            })
    }
}

fn format_usage_value(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "未知".to_owned())
}

fn format_duration_value(value: Option<u64>) -> String {
    match value {
        Some(milliseconds) if milliseconds >= 1_000 => format!(
            "{}.{:01}s",
            milliseconds / 1_000,
            (milliseconds % 1_000) / 100
        ),
        Some(milliseconds) => format!("{milliseconds}ms"),
        None => "未知".to_owned(),
    }
}
