//! 会话正文：Markdown、reasoning、工具生命周期和待决交互均来自 typed facts。

use std::rc::Rc;

use chrono::Timelike;
use ely_gpui_component::{
    agent::HumanInputRequest,
    buttons::{Button, ButtonVariant, IconButton},
    chat::StreamingMarkdown,
    documents::MarkdownRenderer,
    forms::{Checkbox, Input, TextInput},
    layout::Collapsible,
    menus::{Menu, MenuItem, OverflowMenu},
    primitives::{Disclosure, FocusRing, Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius},
};
use gpui::{
    AnyElement, App, Entity, Focusable, InteractiveElement, IntoElement, ListState, MouseButton,
    ParentElement, Pixels, RenderOnce, Role as AriaRole, StatefulInteractiveElement, Styled,
    Window, div, list, prelude::*, px, relative, svg,
};

use super::{
    model::{
        AssistantFeedback, ConversationFact, FileChangeOperation, MessageBlock, MessageRole,
        NativeUiAction, ToolFact, ToolStatus, UiMessage,
    },
    style::{
        EMPTY_COMPOSER_MAX_WIDTH, TITLEBAR_HEIGHT, UiTextSize, code_text_size, mono_font,
        ui_text_size,
    },
};

type ActionHandler = Rc<dyn Fn(NativeUiAction, &mut Window, &mut App)>;
type DraftSuggestionHandler = Rc<dyn Fn(&mut Window, &mut App)>;

const EMPTY_GREETING_MIN_FONT_SIZE_PX: f32 = 20.0;
const EMPTY_GREETING_MAX_FONT_SIZE_PX: f32 = 30.0;
const EMPTY_GREETING_HORIZONTAL_PADDING_PX: f32 = 32.0;
const EMPTY_GREETING_TOP_BASIS: f32 = 0.29;
const EMPTY_GREETING_TOP_MIN_HEIGHT_PX: f32 = 52.0;
const EMPTY_GREETING_BOTTOM_MARGIN_PX: f32 = 32.0;
const EMPTY_DOCK_TOP_MARGIN_PX: f32 = 12.0;
const EMPTY_DOCK_BOTTOM_PADDING_PX: f32 = 16.0;
const FIRST_TURN_TOP_PADDING_PX: f32 = 56.0;
const MESSAGE_USER_MAX_WIDTH_PX: f32 = 576.0;

fn draft_greeting() -> &'static str {
    match chrono::Local::now().hour() {
        5..9 => "早上好呀，新的一天开始啦",
        9..12 => "上午好呀，有什么想让我帮忙的吗",
        12..14 => "中午好呀，要不要先休息一下",
        14..18 => "下午好呀，接下来交给我吧",
        18..23 => "晚上好呀，今天辛苦啦",
        _ => "夜深啦，别忘了照顾好自己哦",
    }
}

/// 空态标题只在自然宽度撞上输入列时缩小，避免窄窗口中的中文标题换成多行。
fn greeting_font_size(
    greeting: &str,
    available_width: Pixels,
    window: &mut Window,
    font_scale: f32,
) -> Pixels {
    let max_size = px(EMPTY_GREETING_MAX_FONT_SIZE_PX * font_scale);
    let min_size = px(EMPTY_GREETING_MIN_FONT_SIZE_PX * font_scale);
    let available_width = f32::from(available_width).max(0.0);
    if available_width <= 0.0 {
        return max_size;
    }

    let run = window.text_style().to_run(greeting.len());
    let natural_width = window
        .text_system()
        .shape_line(greeting.into(), max_size, &[run], None)
        .width;
    let natural_width = f32::from(natural_width);
    if natural_width <= available_width || natural_width <= 0.0 {
        return max_size;
    }

    px((f32::from(max_size) * available_width / natural_width)
        .clamp(f32::from(min_size), f32::from(max_size)))
}

/// 空态建议只更新共享草稿实体，发送仍由 Composer 的既有提交交互负责。
fn draft_suggestion_chip(
    id: &'static str,
    label: &'static str,
    icon: IconName,
    field: &Entity<TextInput>,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let activate: DraftSuggestionHandler = {
        let field = field.clone();
        Rc::new(move |window, cx| {
            field.update(cx, |field, cx| field.set_text(label, cx));
            window.focus(&field.read(cx).focus_handle(cx), cx);
        })
    };
    let click_activate = activate.clone();
    let key_activate = activate.clone();
    let hover = colors.hover;
    let active = colors.active;
    let chip_group = format!("draft-suggestion-{label}");
    let chip_foreground = colors.fg;

    div()
        .id(id)
        .role(AriaRole::Button)
        .aria_label(label)
        .tab_index(0)
        .h(px(32.0))
        .flex_none()
        .flex()
        .items_center()
        .gap_1p5()
        .px(px(12.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(colors.border)
        .text_size(ui_text_size(theme, UiTextSize::Base))
        .font_weight(gpui::FontWeight::NORMAL)
        .text_color(colors.fg)
        .group(chip_group.clone())
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(active))
        .focus_ring(cx)
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |_, window, cx| click_activate(window, cx))
        .on_key_down(move |event, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                cx.stop_propagation();
                key_activate(window, cx);
            }
        })
        // 来源只降低图标和文字透明度，边框保持完整；悬停时恢复前景色。
        .child(
            Icon::new(icon)
                .size(IconSize::Md)
                .color(chip_foreground.opacity(0.7))
                .group_hover_color(chip_group.clone(), chip_foreground),
        )
        .child(
            div()
                .text_color(chip_foreground.opacity(0.7))
                .group_hover(chip_group, |style| style.text_color(chip_foreground))
                .child(label),
        )
        .into_any_element()
}

fn draft_suggestion_row(field: &Entity<TextInput>, cx: &mut App) -> AnyElement {
    let chips = vec![
        draft_suggestion_chip(
            "draft-suggestion-weekly",
            "周报总结",
            IconName::AlarmClock,
            field,
            cx,
        ),
        draft_suggestion_chip(
            "draft-suggestion-bugfix",
            "报错修复",
            IconName::Bug,
            field,
            cx,
        ),
        draft_suggestion_chip(
            "draft-suggestion-review",
            "代码审查",
            IconName::Code,
            field,
            cx,
        ),
        draft_suggestion_chip(
            "draft-suggestion-offpeak",
            "闲时任务",
            IconName::Moon,
            field,
            cx,
        ),
    ];
    div()
        .mt(px(24.0))
        .w_full()
        .flex()
        .flex_wrap()
        .justify_center()
        .gap_4()
        .children(chips)
        .into_any_element()
}

/// 当前 Conversation 的正文视图。历史由宿主分页，组件只渲染当前有界窗口。
#[derive(IntoElement)]
pub struct ChatView {
    conversation: Option<ConversationFact>,
    conversation_width: Pixels,
    empty_composer: Option<AnyElement>,
    draft_field: Option<Entity<TextInput>>,
    on_action: ActionHandler,
    message_list: ListState,
}

impl ChatView {
    pub fn new(
        conversation: Option<ConversationFact>,
        conversation_width: Pixels,
        empty_composer: Option<AnyElement>,
        on_action: impl Fn(NativeUiAction, &mut Window, &mut App) + 'static,
        message_list: ListState,
    ) -> Self {
        Self {
            conversation,
            conversation_width,
            empty_composer,
            draft_field: None,
            on_action: Rc::new(on_action),
            message_list,
        }
    }

    pub fn with_draft_field(mut self, field: &Entity<TextInput>) -> Self {
        self.draft_field = Some(field.clone());
        self
    }

    fn message(
        &self,
        message: &UiMessage,
        session_id: &str,
        transcript_revision: u64,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let message_id = message.message_id.clone();
        let (colors, body_text_size, user_radius) = {
            let theme = cx.theme();
            let body_text_size = match message.role {
                MessageRole::System => ui_text_size(theme, UiTextSize::Xs),
                MessageRole::User | MessageRole::Assistant => ui_text_size(theme, UiTextSize::Base),
            };
            (
                theme.colors.clone(),
                body_text_size,
                theme.radius(Radius::Xl),
            )
        };
        let mut body = div()
            .id(format!("message-body:{message_id}"))
            .w_full()
            .min_w_0()
            .max_w_full()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(body_text_size)
            .line_height(relative(1.6));
        for block in &message.blocks {
            body = body.child(self.block(block, session_id.to_owned(), window, cx));
        }
        match message.role {
            MessageRole::User => div()
                .id(format!("message-user:{message_id}"))
                .w_full()
                .flex()
                .justify_end()
                .child(
                    div()
                        .min_w_0()
                        .max_w_full()
                        .max_w(px(MESSAGE_USER_MAX_WIDTH_PX))
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.surface)
                        .rounded(user_radius)
                        .px_4()
                        .py_3()
                        .text_color(colors.fg)
                        .child(body),
                )
                .into_any_element(),
            MessageRole::Assistant => {
                let message_group = format!("message:{message_id}");
                div()
                    .id(format!("message-assistant:{message_id}"))
                    .group(message_group.clone())
                    .w_full()
                    .min_w_0()
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_color(colors.fg)
                    .child(body)
                    .child(self.feedback_row(
                        session_id.to_owned(),
                        message_id,
                        message.feedback,
                        transcript_revision,
                        window,
                        cx,
                    ))
                    .into_any_element()
            }
            MessageRole::System => div()
                .id(format!("message-system:{message_id}"))
                .w_full()
                .min_w_0()
                .max_w_full()
                .flex()
                .justify_center()
                .py_1()
                .text_color(colors.fg_muted)
                .child(body)
                .into_any_element(),
        }
    }

    fn block(
        &self,
        block: &MessageBlock,
        session_id: String,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = &theme.colors;
        match block {
            MessageBlock::Markdown {
                block_id,
                source,
                streaming,
            } => {
                if *streaming {
                    // 流式阶段只绘制原文；累计 Markdown 交给完成后的单次解析，避免每个
                    // token 都重新构造代码块、表格和链接树。
                    div()
                        .id(format!("markdown-stream:{block_id}"))
                        .w_full()
                        .min_w_0()
                        .max_w_full()
                        .child(source.clone())
                        .into_any_element()
                } else {
                    div()
                        .w_full()
                        .min_w_0()
                        .max_w_full()
                        .child(
                            StreamingMarkdown::new(
                                format!("markdown:{block_id}"),
                                source.clone(),
                                false,
                            )
                            .into_any_element(),
                        )
                        .into_any_element()
                }
            }
            MessageBlock::Reasoning {
                block_id,
                source,
                streaming,
            } => {
                let body = if *streaming {
                    div()
                        .id(format!("reasoning-stream:{block_id}"))
                        .w_full()
                        .min_w_0()
                        .max_w_full()
                        .child(source.clone())
                        .into_any_element()
                } else {
                    div()
                        .w_full()
                        .min_w_0()
                        .max_w_full()
                        .child(
                            StreamingMarkdown::new(
                                format!("reasoning:{block_id}"),
                                source.clone(),
                                false,
                            )
                            .into_any_element(),
                        )
                        .into_any_element()
                };
                div()
                    .w_full()
                    .min_w_0()
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .rounded(theme.radius(Radius::Md))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.sunken)
                    .text_size(ui_text_size(theme, UiTextSize::Sm))
                    .text_color(colors.fg_muted)
                    .child(div().font_weight(gpui::FontWeight::MEDIUM).child("思考"))
                    .child(body)
                    .into_any_element()
            }
            MessageBlock::Tool(tool) => self.tool(tool, window, cx),
            MessageBlock::Approval(approval) => {
                let request_id = approval.request_id.clone();
                let on_action = self.on_action.clone();
                let approve_id = request_id.clone();
                let deny_id = request_id.clone();
                let approve_session = session_id.clone();
                let deny_session = session_id;
                // 审批参数来自工具原文，不能截断；先锁定卡片宽度，再用双向滚动承载长 JSON，
                // 避免 flex 的 intrinsic width 把标题和操作按钮推出消息列或视口。
                div()
                    .w_full()
                    .min_w_0()
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded(theme.radius(Radius::Md))
                    .border_1()
                    .border_color(colors.warning)
                    .bg(colors.warning.opacity(0.08))
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .max_w_full()
                            .flex()
                            .items_start()
                            .gap_2()
                            .text_color(colors.fg)
                            .child(Icon::new(IconName::Shield).size(IconSize::Sm))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .max_w_full()
                                    .child(format!("工具需要批准：{}", approval.name)),
                            ),
                    )
                    .child(
                        div()
                            .id(format!("approval-arguments:{request_id}"))
                            .w_full()
                            .min_w_0()
                            .max_w_full()
                            .max_h(px(240.0))
                            .overflow_scroll()
                            .font(mono_font(theme.mono_family.clone()))
                            // 审批参数是 JSON 代码内容，跟随代码字号设置而不是界面字号。
                            .text_size(code_text_size(theme))
                            .text_color(colors.fg_muted)
                            .child(approval.arguments_json.clone()),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .max_w_full()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new(format!("tool-approve:{request_id}"), "允许")
                                    .variant(ButtonVariant::Primary)
                                    .size(ControlSize::Sm)
                                    .disabled(!approval.can_approve)
                                    .on_click(move |_, window, cx| {
                                        on_action(
                                            NativeUiAction::ApproveTool {
                                                session_id: approve_session.clone(),
                                                request_id: approve_id.clone(),
                                                approved: true,
                                                operation_id: format!("ui:approve:{approve_id}"),
                                            },
                                            window,
                                            cx,
                                        )
                                    }),
                            )
                            .child({
                                let on_action = self.on_action.clone();
                                Button::new(format!("tool-deny:{request_id}"), "拒绝")
                                    .variant(ButtonVariant::Danger)
                                    .size(ControlSize::Sm)
                                    .disabled(!approval.can_deny)
                                    .on_click(move |_, window, cx| {
                                        on_action(
                                            NativeUiAction::ApproveTool {
                                                session_id: deny_session.clone(),
                                                request_id: deny_id.clone(),
                                                approved: false,
                                                operation_id: format!("ui:deny:{deny_id}"),
                                            },
                                            window,
                                            cx,
                                        )
                                    })
                            }),
                    )
                    .into_any_element()
            }
            MessageBlock::Question(question) => {
                let request_id = question.request_id.clone();
                if question.multi_select {
                    let initial_selection = question
                        .answer
                        .as_deref()
                        .and_then(|answer| serde_json::from_str::<Vec<String>>(answer).ok())
                        .unwrap_or_default();
                    let selected = window.use_keyed_state(
                        format!("question-selection:{request_id}"),
                        cx,
                        move |_, _| initial_selection,
                    );
                    let selected_values = selected.read(cx).clone();
                    let choices = question.choices.iter().map(|choice| {
                        let id = choice.id.clone();
                        let selected = selected.clone();
                        Checkbox::new(
                            format!("question-choice:{request_id}:{id}"),
                            selected_values.iter().any(|value| value == &id),
                        )
                        .label(choice.label.clone())
                        .disabled(question.answered)
                        .on_change(move |checked, _, cx| {
                            selected.update(cx, |values, cx| {
                                if checked {
                                    if !values.iter().any(|value| value == &id) {
                                        values.push(id.clone());
                                    }
                                } else {
                                    values.retain(|value| value != &id);
                                }
                                cx.notify();
                            });
                        })
                    });
                    let freeform = question.allow_freeform.then(|| {
                        window.use_keyed_state(
                            format!("question-freeform:{request_id}"),
                            cx,
                            |window, cx| TextInput::new(window, cx).placeholder("补充说明"),
                        )
                    });
                    let on_action = self.on_action.clone();
                    let submit_values = selected.clone();
                    let submit_freeform = freeform.clone();
                    let submit_id = request_id.clone();
                    let submit_session = session_id.clone();
                    let submit = Button::new(format!("question-submit:{request_id}"), "提交")
                        .variant(ButtonVariant::Primary)
                        .size(ControlSize::Sm)
                        .disabled(question.answered)
                        .on_click(move |_, window, cx| {
                            let answer = serde_json::to_string(&submit_values.read(cx).clone())
                                .map(|answer| {
                                    if let Some(field) = submit_freeform.as_ref() {
                                        let freeform = field.read(cx).text().trim().to_owned();
                                        if !freeform.is_empty() {
                                            let mut values = submit_values.read(cx).clone();
                                            values.push(freeform);
                                            return serde_json::to_string(&values)
                                                .unwrap_or(answer);
                                        }
                                    }
                                    answer
                                })
                                .unwrap_or_else(|_| "[]".to_owned());
                            on_action(
                                NativeUiAction::AnswerQuestion {
                                    session_id: submit_session.clone(),
                                    request_id: submit_id.clone(),
                                    answer,
                                    operation_id: format!("ui:answer:{}", submit_id),
                                },
                                window,
                                cx,
                            );
                        });
                    let (radius, prompt_text_size, colors) = {
                        let theme = cx.theme();
                        (
                            theme.radius(Radius::Lg),
                            ui_text_size(theme, UiTextSize::Sm),
                            theme.colors.clone(),
                        )
                    };
                    let answer = question
                        .answer
                        .clone()
                        .unwrap_or_else(|| "已回答".to_owned());
                    if let Some(field) = freeform.as_ref() {
                        let answered = question.answered;
                        if field.read(cx).is_disabled() != answered {
                            field.update(cx, |input, cx| input.set_disabled(answered, cx));
                        }
                    }
                    let choice_list = div()
                        .w_full()
                        .min_w_0()
                        .max_w_full()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(choices);
                    let mut card = div()
                        .w_full()
                        .min_w_0()
                        .max_w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_4()
                        .rounded(radius)
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.surface)
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .max_w_full()
                                .text_size(prompt_text_size)
                                .text_color(colors.fg)
                                .child(question.prompt_markdown.clone()),
                        )
                        .child(choice_list);
                    if let Some(field) = freeform {
                        card = card.child(
                            div()
                                .w_full()
                                .min_w_0()
                                .max_w_full()
                                .child(Input::new(&field)),
                        );
                    }
                    card.when(question.answered, |card| {
                        card.child(
                            div()
                                .w_full()
                                .min_w_0()
                                .max_w_full()
                                .text_color(colors.fg_muted)
                                .child(answer.clone()),
                        )
                    })
                    .when(!question.answered, |card| card.child(submit))
                    .into_any_element()
                } else {
                    let field = window.use_keyed_state(
                        format!("question:{request_id}"),
                        cx,
                        |window, cx| TextInput::new(window, cx).placeholder("输入回答"),
                    );
                    let on_action = self.on_action.clone();
                    let request_id_for_answer = request_id.clone();
                    let mut request = HumanInputRequest::new(
                        format!("question-request:{request_id}"),
                        question.prompt_markdown.clone(),
                        &field,
                        move |answer, window, cx| {
                            on_action(
                                NativeUiAction::AnswerQuestion {
                                    session_id: session_id.clone(),
                                    request_id: request_id_for_answer.clone(),
                                    answer: answer.to_owned(),
                                    operation_id: format!("ui:answer:{}", request_id_for_answer),
                                },
                                window,
                                cx,
                            )
                        },
                    )
                    .choices(question.choices.iter().map(|choice| choice.label.clone()));
                    if question.answered {
                        request = request.answered(
                            question
                                .answer
                                .clone()
                                .unwrap_or_else(|| "已回答".to_owned()),
                        );
                    }
                    div()
                        .w_full()
                        .min_w_0()
                        .max_w_full()
                        .child(request.into_any_element())
                        .into_any_element()
                }
            }
            MessageBlock::FileChange(change) => {
                let icon = match change.operation {
                    FileChangeOperation::Create => IconName::FilePlus,
                    FileChangeOperation::Modify => IconName::FileText,
                    FileChangeOperation::Delete => IconName::File,
                };
                div()
                    .w_full()
                    .min_w_0()
                    .max_w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded(theme.radius(Radius::Sm))
                    .bg(colors.sunken)
                    .text_size(ui_text_size(theme, UiTextSize::Sm))
                    .text_color(colors.fg_muted)
                    .child(Icon::new(icon).size(IconSize::Sm))
                    .child(
                        // 文件路径属于代码内容，卡片状态文案仍继承界面字号。
                        div()
                            .flex_1()
                            .min_w_0()
                            .font(mono_font(theme.mono_family.clone()))
                            .text_size(code_text_size(theme))
                            .child(change.path.clone()),
                    )
                    .child(div().flex_shrink_0().child(if !change.applied {
                        "待应用"
                    } else if change.rewindable {
                        "已应用"
                    } else {
                        "已撤销"
                    }))
                    .into_any_element()
            }
            MessageBlock::Attachment(attachment) => div()
                .w_full()
                .min_w_0()
                .max_w_full()
                .flex()
                .items_center()
                .gap_2()
                .p_2()
                .rounded(theme.radius(Radius::Sm))
                .border_1()
                .border_color(colors.border)
                .child(
                    Icon::new(if attachment.image {
                        IconName::Image
                    } else {
                        IconName::Paperclip
                    })
                    .size(IconSize::Sm),
                )
                .child(div().flex_1().min_w_0().child(attachment.file_name.clone()))
                .child(
                    div()
                        .flex_shrink_0()
                        .child(format!("{} B", attachment.bytes)),
                )
                .into_any_element(),
            MessageBlock::Error {
                block_id,
                code,
                message,
            } => div()
                .id(format!("message-error:{block_id}"))
                .w_full()
                .min_w_0()
                .max_w_full()
                .flex()
                .flex_col()
                .gap_1()
                .p_3()
                .rounded(theme.radius(Radius::Md))
                .border_1()
                .border_color(colors.danger)
                .text_color(colors.danger)
                .child(div().w_full().min_w_0().max_w_full().child(code.clone()))
                .child(div().w_full().min_w_0().max_w_full().child(message.clone()))
                .into_any_element(),
        }
    }

    fn tool(&self, tool: &ToolFact, window: &mut Window, cx: &mut App) -> AnyElement {
        let (colors, base_text_size, small_text_size, small_radius, mono_family) = {
            let theme = cx.theme();
            (
                theme.colors.clone(),
                ui_text_size(theme, UiTextSize::Base),
                ui_text_size(theme, UiTextSize::Xs),
                theme.radius(Radius::Sm),
                theme.mono_family.clone(),
            )
        };
        let status = match tool.status {
            ToolStatus::Requested => "已请求",
            ToolStatus::Running => "运行中",
            ToolStatus::Succeeded => "完成",
            ToolStatus::Failed => "失败",
            ToolStatus::Cancelled => "已取消",
            ToolStatus::SideEffectUnknown => "结果未知",
        };
        let tint = match tool.status {
            ToolStatus::Succeeded => colors.success,
            ToolStatus::Failed | ToolStatus::SideEffectUnknown => colors.danger,
            ToolStatus::Running | ToolStatus::Requested => colors.focus,
            ToolStatus::Cancelled => colors.fg_muted,
        };
        let tool_request_id = tool.request_id.clone();
        let open_state =
            window.use_keyed_state(format!("tool-open:{tool_request_id}"), cx, |_, _| false);
        let open = *open_state.read(cx);
        let has_arguments = !tool.arguments_json.trim().is_empty();
        let output = tool
            .output_markdown
            .clone()
            .filter(|output| !output.trim().is_empty());
        let opens = has_arguments || output.is_some();
        let failed = matches!(
            tool.status,
            ToolStatus::Failed | ToolStatus::SideEffectUnknown
        );

        let toggle_click = open_state.clone();
        let toggle_key = open_state.clone();
        let header_id = format!("tool-header:{tool_request_id}");
        let mut header = div()
            .id(header_id)
            .w_full()
            .min_w_0()
            .max_w_full()
            .flex()
            .items_center()
            .gap_2()
            .px_1()
            .py_0p5()
            .text_size(base_text_size)
            .text_color(colors.fg)
            .when(opens, |header| {
                header
                    .role(AriaRole::Button)
                    .aria_label(if open {
                        "收起工具详情"
                    } else {
                        "展开工具详情"
                    })
                    .tab_index(0)
                    .focus_ring(cx)
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.hover))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(move |_, _, cx| {
                        toggle_click.update(cx, |open, cx| {
                            *open = !*open;
                            cx.notify();
                        });
                    })
                    .on_key_down(move |event, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            cx.stop_propagation();
                            toggle_key.update(cx, |open, cx| {
                                *open = !*open;
                                cx.notify();
                            });
                        }
                    })
            })
            .child(Icon::new(IconName::Wrench).size(IconSize::Sm).color(tint))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .max_w_full()
                    .child(tool.name.clone()),
            )
            .child(div().flex_shrink_0().text_color(tint).child(status));
        if opens {
            header = header.child(
                Disclosure::new(format!("tool-disclosure:{tool_request_id}"), open)
                    .size(IconSize::Sm)
                    .color(colors.fg_subtle),
            );
        }

        let mut details = div()
            .w_full()
            .min_w_0()
            .max_w_full()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .pt_1()
            .pb_3();
        if has_arguments {
            details = details.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(small_text_size)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(colors.fg_subtle)
                            .child("参数"),
                    )
                    .child(
                        div()
                            .id(format!("tool-arguments:{tool_request_id}"))
                            .w_full()
                            .min_w_0()
                            .max_w_full()
                            .p_2()
                            .max_h(px(240.0))
                            .overflow_scroll()
                            .rounded(small_radius)
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.sunken)
                            .font(mono_font(mono_family.clone()))
                            .text_size(small_text_size)
                            .text_color(colors.fg_muted)
                            .child(tool.arguments_json.clone()),
                    ),
            );
        }
        if let Some(output) = output {
            let output_label = if failed { "错误" } else { "输出" };
            details = details.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(small_text_size)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(colors.fg_subtle)
                            .child(output_label),
                    )
                    .child(
                        div()
                            .id(format!("tool-output-scroll:{tool_request_id}"))
                            .w_full()
                            .min_w_0()
                            .max_w_full()
                            .p_2()
                            .max_h(px(480.0))
                            .overflow_y_scroll()
                            .rounded(small_radius)
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.surface)
                            .child(
                                MarkdownRenderer::new(
                                    format!("tool-output:{tool_request_id}"),
                                    output,
                                )
                                .into_any_element(),
                            ),
                    ),
            );
        }

        // 工具摘要默认折叠；详情面板只在用户展开后挂载，避免历史消息永久撑高。
        div()
            .w_full()
            .min_w_0()
            .max_w_full()
            .flex()
            .flex_col()
            .gap_2()
            .child(header)
            .when(opens, |body| {
                body.child(
                    div().w_full().min_w_0().max_w_full().child(
                        Collapsible::new(format!("tool-details:{tool_request_id}"), open)
                            .child(details),
                    ),
                )
            })
            .into_any_element()
    }

    fn feedback_row(
        &self,
        session_id: String,
        message_id: String,
        feedback: Option<AssistantFeedback>,
        expected_transcript_revision: u64,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let message_group = format!("message:{message_id}");
        let focus = window
            .use_keyed_state(
                format!("message-feedback-focus:{message_id}"),
                cx,
                |_, cx| cx.focus_handle(),
            )
            .read(cx)
            .clone();
        let on_action = self.on_action.clone();
        let like_action = on_action.clone();
        let dislike_action = on_action.clone();
        let like_id = message_id.clone();
        let dislike_id = message_id.clone();
        let like_session = session_id.clone();
        let dislike_session = session_id;
        div()
            // 反馈容器不参与 Tab 顺序；子按钮获得键盘焦点时仍恢复整行可见。
            .track_focus(&focus)
            .in_focus(|style| style.opacity(1.0))
            .opacity(0.0)
            .group_hover(message_group, |style| style.opacity(1.0))
            .flex()
            .gap_1()
            .mt_1()
            .child(
                IconButton::new(format!("feedback-like:{message_id}"), IconName::ThumbsUp)
                    .variant(if feedback == Some(AssistantFeedback::Like) {
                        ButtonVariant::Subtle
                    } else {
                        ButtonVariant::Ghost
                    })
                    .size(ControlSize::Sm)
                    .tooltip("有帮助")
                    .on_click(move |_, window, cx| {
                        like_action(
                            NativeUiAction::Feedback {
                                session_id: like_session.clone(),
                                message_id: like_id.clone(),
                                feedback: Some(AssistantFeedback::Like),
                                expected_transcript_revision,
                                operation_id: format!("ui:feedback:like:{like_id}"),
                            },
                            window,
                            cx,
                        )
                    }),
            )
            .child(
                IconButton::new(
                    format!("feedback-dislike:{message_id}"),
                    IconName::ThumbsDown,
                )
                .variant(if feedback == Some(AssistantFeedback::Dislike) {
                    ButtonVariant::Subtle
                } else {
                    ButtonVariant::Ghost
                })
                .size(ControlSize::Sm)
                .tooltip("需要改进")
                .on_click(move |_, window, cx| {
                    dislike_action(
                        NativeUiAction::Feedback {
                            session_id: dislike_session.clone(),
                            message_id: dislike_id.clone(),
                            feedback: Some(AssistantFeedback::Dislike),
                            expected_transcript_revision,
                            operation_id: format!("ui:feedback:dislike:{dislike_id}"),
                        },
                        window,
                        cx,
                    )
                }),
            )
            .into_any_element()
    }
}

impl RenderOnce for ChatView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ChatView {
            conversation,
            conversation_width,
            on_action,
            message_list,
            empty_composer,
            draft_field,
        } = self;
        let colors = {
            let theme = cx.theme();
            theme.colors.clone()
        };
        let conversation_is_empty = conversation.as_ref().is_none_or(|conversation| {
            conversation.messages.is_empty() && conversation.input_queue.is_empty()
        });
        if conversation_is_empty {
            let greeting = draft_greeting();
            let greeting_width = px((f32::from(conversation_width).min(EMPTY_COMPOSER_MAX_WIDTH)
                - EMPTY_GREETING_HORIZONTAL_PADDING_PX)
                .max(0.0));
            let greeting_size =
                greeting_font_size(greeting, greeting_width, window, cx.theme().font_scale);
            // 29dvh 顶部占位让问候与 composer 形成来源的非对称空态布局；
            // 最小高度避免小窗口中标题直接贴到标题栏下方。
            let top_spacer = px((f32::from(window.viewport_size().height)
                * EMPTY_GREETING_TOP_BASIS)
                .max(EMPTY_GREETING_TOP_MIN_HEIGHT_PX));
            let empty_state = div()
                .flex()
                .w_full()
                .max_w(px(EMPTY_COMPOSER_MAX_WIDTH))
                .flex_shrink_0()
                .relative()
                .items_center()
                .justify_center()
                // 水印 SVG 没有交互监听，GPUI 不会为它创建阻断命中框。
                .child(
                    div()
                        .absolute()
                        .top(px(-182.0))
                        .left(relative(0.5))
                        .ml(px(-200.0))
                        .w(px(400.0))
                        .h(px(320.0))
                        .child(
                            svg()
                                .path("native/brand/empty-watermark.svg")
                                .size_full()
                                .text_color(colors.fg_subtle.opacity(0.7)),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .mb(px(EMPTY_GREETING_BOTTOM_MARGIN_PX))
                        .child(
                            div()
                                .w_full()
                                .px_4()
                                .text_color(colors.fg)
                                .text_center()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_size(greeting_size)
                                .line_height(relative(1.2))
                                .child(greeting),
                        ),
                );
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .px_4()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .child(
                            div()
                                .w_full()
                                .flex_shrink_1()
                                .h(top_spacer)
                                .min_h(px(EMPTY_GREETING_TOP_MIN_HEIGHT_PX)),
                        )
                        .child(empty_state)
                        .when_some(empty_composer, |body, composer| {
                            body.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_shrink_0()
                                    .mt(px(EMPTY_DOCK_TOP_MARGIN_PX))
                                    .w_full()
                                    .max_w(px(EMPTY_COMPOSER_MAX_WIDTH))
                                    .mx_auto()
                                    .child(composer)
                                    .when_some(draft_field, |body, field| {
                                        body.child(draft_suggestion_row(&field, cx))
                                    })
                                    .pb(px(EMPTY_DOCK_BOTTOM_PADDING_PX)),
                            )
                        })
                        .child(div().w_full().flex_1().min_h_0()),
                )
                .into_any_element();
        }
        let conversation = conversation.expect("非空 ChatView 必须持有会话");
        let session_id = conversation.session.session_id.clone();
        let history_cursor = conversation.history.clone();
        let rewind_turn_id = conversation
            .messages
            .iter()
            .rev()
            .find_map(|message| message.turn_id.clone());
        let can_rewind = rewind_turn_id.is_some();
        let transcript_revision = conversation.transcript_revision;
        let message_sources = conversation.messages;
        let message_session_id = session_id.clone();
        let message_renderer = ChatView {
            conversation: None,
            conversation_width,
            empty_composer: None,
            draft_field: None,
            on_action: on_action.clone(),
            message_list: message_list.clone(),
        };
        let messages = list(message_list, move |index, window, cx| {
            let Some(message) = message_sources.get(index) else {
                return div().into_any_element();
            };
            div()
                .pb_5()
                .child(message_renderer.message(
                    message,
                    &message_session_id,
                    transcript_revision,
                    window,
                    cx,
                ))
                .into_any_element()
        })
        .size_full();
        let branch = on_action.clone();
        let rewind = on_action.clone();
        let restore = on_action.clone();
        let session_for_branch = session_id.clone();
        let session_for_rewind = session_id.clone();
        let session_for_restore = session_id.clone();
        let rewind_turn_id_for_action = rewind_turn_id.clone();
        let status = conversation.session.status;
        let branch_item = MenuItem::new("分支会话")
            .icon(IconName::GitBranch)
            .on_click(move |window, cx| {
                branch(
                    NativeUiAction::Branch {
                        session_id: session_for_branch.clone(),
                        through_turn_id: None,
                        operation_id: format!("ui:branch:{}", session_for_branch),
                    },
                    window,
                    cx,
                )
            });
        let rewind_item = MenuItem::new("回退")
            .icon(IconName::Undo2)
            .disabled(!can_rewind)
            .on_click(move |window, cx| {
                let Some(target_turn_id) = rewind_turn_id_for_action.clone() else {
                    return;
                };
                rewind(
                    NativeUiAction::Rewind {
                        session_id: session_for_rewind.clone(),
                        target_turn_id,
                        operation_id: format!("ui:rewind:{}", session_for_rewind),
                    },
                    window,
                    cx,
                )
            });
        let mut session_menu = Menu::new().item(branch_item).item(rewind_item);
        if status == super::model::SessionStatus::Corrupt {
            session_menu = session_menu.item(
                MenuItem::new("恢复会话")
                    .icon(IconName::RotateCcw)
                    .on_click(move |window, cx| {
                        restore(
                            NativeUiAction::ColdRestore {
                                session_id: session_for_restore.clone(),
                                operation_id: format!("ui:cold-restore:{}", session_for_restore),
                            },
                            window,
                            cx,
                        )
                    }),
            );
        }
        if let Some(cursor) = history_cursor {
            let action = on_action.clone();
            let history_session_id = session_id.clone();
            let latest_action = on_action.clone();
            let latest_session_id = session_id.clone();
            session_menu = session_menu
                .item(
                    MenuItem::new("加载更早消息")
                        .icon(IconName::ArrowUp)
                        .on_click(move |window, cx| {
                            action(
                                NativeUiAction::LoadHistory {
                                    session_id: history_session_id.clone(),
                                    cursor: cursor.clone(),
                                },
                                window,
                                cx,
                            )
                        }),
                )
                .item(
                    MenuItem::new("回到最新消息")
                        .icon(IconName::ArrowDown)
                        .on_click(move |window, cx| {
                            latest_action(
                                NativeUiAction::LoadLatest {
                                    session_id: latest_session_id.clone(),
                                },
                                window,
                                cx,
                            )
                        }),
                );
        }
        let session_actions = div()
            .absolute()
            .top(px(TITLEBAR_HEIGHT + 4.0))
            .left_2()
            .child(OverflowMenu::new("chat-session-actions", session_menu).tooltip("会话操作"));
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .bg(colors.bg)
            .child(
                div()
                    .id("native-chat-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .px_6()
                    .py_4()
                    .child(
                        div()
                            // 虚拟列表使用 size_full；外层必须建立列向 flex 约束才能继承可用高度。
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .max_w(conversation_width)
                            .mx_auto()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_h_0()
                                    .pt(px(FIRST_TURN_TOP_PADDING_PX))
                                    .child(messages),
                            ),
                    ),
            )
            .child(session_actions)
            .into_any_element()
    }
}
