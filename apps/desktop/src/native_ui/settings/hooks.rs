//! Hooks 设置页。
//!
//! 用户和项目 Hook 允许编辑；插件 Hook 只展示静态诊断。所有写入都通过
//! `SettingsCommand` 进入 NativeSettingsDomain，页面不直接访问配置文件。

use super::{
    SettingsCommandHandler, SettingsRow, SettingsSection, contracts::*,
    section::settings_switch_style,
};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{Choice, Input, Select, Switch, TextInput},
    theme::ActiveTheme,
};
use gpui::{AnyElement, App, Entity, IntoElement, ParentElement, Styled, Window, div, px};
use serde_json::{self, Value};
use std::cell::Cell;
use std::rc::Rc;

fn text_input(
    window: &mut Window,
    cx: &mut App,
    key: impl Into<gpui::ElementId>,
    initial: impl Into<String>,
    placeholder: &'static str,
) -> Entity<TextInput> {
    window.use_keyed_state(key, cx, move |window, cx| {
        let mut input = TextInput::new(window, cx)
            .label(placeholder)
            .placeholder(placeholder);
        input.set_text(initial.into(), cx);
        input
    })
}

fn set_input(input: &Entity<TextInput>, value: impl Into<String>, cx: &mut App) {
    input.update(cx, |input, cx| input.set_text(value.into(), cx));
}

fn hook_select(
    id: &'static str,
    label: &'static str,
    input: Entity<TextInput>,
    choices: impl IntoIterator<Item = Choice>,
    cx: &App,
) -> Select {
    let selected = input.read(cx).text().to_owned();
    Select::new(id, choices)
        .label(label)
        .selected(selected)
        .on_change(move |value, _, cx| set_input(&input, value.to_string(), cx))
}

fn parse_event(value: &str) -> Result<HookEvent, String> {
    HookEvent::ALL
        .into_iter()
        .find(|event| event.as_str().eq_ignore_ascii_case(value.trim()))
        .ok_or_else(|| format!("不支持的 Hook 事件：{}", value.trim()))
}

fn parse_type(value: &str) -> Result<HookType, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "command" => Ok(HookType::Command),
        "context" => Ok(HookType::Context),
        value => Err(format!("不支持的 Hook 类型：{value}")),
    }
}

fn parse_scope(value: &str) -> Result<HookScope, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "user" => Ok(HookScope::User),
        "project" => Ok(HookScope::Project),
        value => Err(format!("不支持的 Hook 作用域：{value}")),
    }
}

fn parse_json_input(value: &str) -> Result<Option<Value>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(value)
        .map(Some)
        .map_err(|error| format!("input 必须是有效 JSON：{error}"))
}

/// 将设置页的可编辑状态集中管理，避免保存、重置和编辑操作分别维护字段列表。
#[derive(Clone)]
struct HookFormFields {
    edit_id: Entity<TextInput>,
    scope: Entity<TextInput>,
    event: Entity<TextInput>,
    hook_type: Entity<TextInput>,
    matcher: Entity<TextInput>,
    command: Entity<TextInput>,
    args: Entity<TextInput>,
    shell: Entity<TextInput>,
    timeout_ms: Entity<TextInput>,
    context: Entity<TextInput>,
    block: Entity<TextInput>,
    input: Entity<TextInput>,
    continue_state: Rc<Cell<bool>>,
}

type HookEditHandler = Rc<dyn Fn(&HookSummary, &gpui::ClickEvent, &mut App)>;

impl HookFormFields {
    fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            edit_id: text_input(window, cx, "settings-hook-edit-id", String::new(), ""),
            scope: text_input(window, cx, "settings-hook-scope", "user", "user 或 project"),
            event: text_input(
                window,
                cx,
                "settings-hook-event",
                "PreToolUse",
                "SessionStart 等事件名",
            ),
            hook_type: text_input(
                window,
                cx,
                "settings-hook-type",
                "command",
                "command 或 context",
            ),
            matcher: text_input(
                window,
                cx,
                "settings-hook-matcher",
                String::new(),
                "可选匹配表达式",
            ),
            command: text_input(
                window,
                cx,
                "settings-hook-command",
                String::new(),
                "可执行命令",
            ),
            args: text_input(
                window,
                cx,
                "settings-hook-args",
                String::new(),
                "每行一个参数",
            ),
            shell: text_input(
                window,
                cx,
                "settings-hook-shell",
                String::new(),
                "bash 或 powershell",
            ),
            timeout_ms: text_input(window, cx, "settings-hook-timeout", "60000", "毫秒"),
            context: text_input(
                window,
                cx,
                "settings-hook-context",
                String::new(),
                "可选追加上下文",
            ),
            block: text_input(
                window,
                cx,
                "settings-hook-block",
                String::new(),
                "PreToolUse 可选阻断消息",
            ),
            input: text_input(
                window,
                cx,
                "settings-hook-input",
                String::new(),
                "PreToolUse 可选 JSON",
            ),
            continue_state: Rc::new(Cell::new(false)),
        }
    }

    fn reset(&self, cx: &mut App) {
        set_input(&self.edit_id, "", cx);
        set_input(&self.scope, "user", cx);
        set_input(&self.event, "PreToolUse", cx);
        set_input(&self.hook_type, "command", cx);
        set_input(&self.matcher, "", cx);
        set_input(&self.command, "", cx);
        set_input(&self.args, "", cx);
        set_input(&self.shell, "", cx);
        set_input(&self.timeout_ms, "60000", cx);
        set_input(&self.context, "", cx);
        set_input(&self.block, "", cx);
        set_input(&self.input, "", cx);
        self.continue_state.set(false);
    }

    fn set_from_summary(&self, hook: &HookSummary, cx: &mut App) {
        let config = config_from_summary(hook);
        set_input(&self.edit_id, hook.id.clone(), cx);
        set_input(&self.scope, hook_scope_name(config.scope), cx);
        set_input(&self.event, config.event.as_str(), cx);
        set_input(
            &self.hook_type,
            match config.hook_type {
                HookType::Command => "command",
                HookType::Context => "context",
            },
            cx,
        );
        set_input(&self.matcher, config.matcher.unwrap_or_default(), cx);
        set_input(&self.command, config.command, cx);
        set_input(&self.args, config.args.join("\n"), cx);
        set_input(&self.shell, config.shell.unwrap_or_default(), cx);
        set_input(&self.timeout_ms, config.timeout_ms.to_string(), cx);
        set_input(&self.context, config.context.unwrap_or_default(), cx);
        set_input(&self.block, config.block.unwrap_or_default(), cx);
        set_input(
            &self.input,
            config
                .input
                .and_then(|value| serde_json::to_string_pretty(&value).ok())
                .unwrap_or_default(),
            cx,
        );
        self.continue_state.set(config.continue_turn);
    }
}

fn config_from_inputs(form: &HookFormFields, cx: &App) -> Result<HookConfig, String> {
    let read = |input: &Entity<TextInput>| input.read(cx).text().trim().to_owned();
    let hook_type = parse_type(&read(&form.hook_type))?;
    let command = read(&form.command);
    if hook_type == HookType::Command && command.is_empty() {
        return Err("command Hook 必须填写 command".to_owned());
    }
    let timeout_ms = read(&form.timeout_ms)
        .parse::<u64>()
        .map_err(|_| "timeout 必须是整数毫秒".to_owned())?;
    if !(1..=3_600_000).contains(&timeout_ms) {
        return Err("timeout 必须在 1 到 3600000 毫秒之间".to_owned());
    }
    Ok(HookConfig {
        scope: parse_scope(&read(&form.scope))?,
        event: parse_event(&read(&form.event))?,
        hook_type,
        matcher: (!read(&form.matcher).is_empty()).then(|| read(&form.matcher)),
        command,
        args: read(&form.args)
            .lines()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        shell: (!read(&form.shell).is_empty()).then(|| read(&form.shell)),
        timeout_ms,
        context: (!read(&form.context).is_empty()).then(|| read(&form.context)),
        block: (!read(&form.block).is_empty()).then(|| read(&form.block)),
        input: parse_json_input(&read(&form.input))?,
        continue_turn: form.continue_state.get(),
        enabled: true,
    })
}

fn config_from_summary(hook: &HookSummary) -> HookConfig {
    HookConfig {
        scope: hook.scope,
        event: hook.event,
        hook_type: hook.hook_type,
        matcher: hook.matcher.clone(),
        command: hook.command.clone(),
        args: hook.args.clone(),
        shell: hook.shell.clone(),
        timeout_ms: hook.timeout_ms,
        context: hook.context.clone(),
        block: hook.block.clone(),
        input: hook.input.clone(),
        continue_turn: hook.continue_turn,
        enabled: hook.enabled,
    }
}

fn hook_scope_name(scope: HookScope) -> &'static str {
    match scope {
        HookScope::User => "user",
        HookScope::Project => "project",
    }
}

fn hook_source_name(source: HookSource) -> &'static str {
    match source {
        HookSource::User => "用户",
        HookSource::Project => "项目",
        HookSource::Plugin => "插件",
    }
}

fn hook_display_command(hook: &HookSummary) -> String {
    match hook.hook_type {
        HookType::Command => hook.command.clone(),
        HookType::Context => hook
            .context
            .as_deref()
            .map(|value| format!("context: {value}"))
            .unwrap_or_else(|| "context Hook".to_owned()),
    }
}

fn render_form(
    current: &HooksSettings,
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let fields = HookFormFields::new(window, cx);
    let form_error = window.use_keyed_state("settings-hook-form-error", cx, |_, _| None::<String>);

    let save_dispatch = dispatch.clone();
    let save_fields = fields.clone();
    let save_form_error = form_error.clone();
    let save = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let config = match config_from_inputs(&save_fields, cx) {
            Ok(config) => config,
            Err(error) => {
                save_form_error.update(cx, |form_error, cx| {
                    *form_error = Some(error);
                    cx.notify();
                });
                return;
            }
        };
        let id = save_fields.edit_id.read(cx).text().trim().to_owned();
        if id.is_empty() {
            save_dispatch(SettingsCommand::CreateHook(config), window, cx);
        } else {
            save_dispatch(
                SettingsCommand::UpdateHook {
                    hook_id: id,
                    scope: config.scope,
                    hook: config,
                },
                window,
                cx,
            );
        }
        save_form_error.update(cx, |form_error, cx| {
            *form_error = None;
            cx.notify();
        });
    };

    let clear_fields = fields.clone();
    let clear_form_error = form_error.clone();
    let clear_form = move |_: &gpui::ClickEvent, _window: &mut Window, cx: &mut App| {
        clear_fields.reset(cx);
        clear_form_error.update(cx, |form_error, cx| {
            *form_error = None;
            cx.notify();
        });
    };

    let edit_fields = fields.clone();
    let edit_row: HookEditHandler =
        Rc::new(move |hook, _, cx| edit_fields.set_from_summary(hook, cx));

    let rows = current
        .hooks
        .iter()
        .map(|hook| {
            let hook_id = hook.id.clone();
            let hook_scope = hook.scope;
            let trust_id = hook.id.clone();
            let delete_id = hook.id.clone();
            let toggle_dispatch = dispatch.clone();
            let trust_dispatch = dispatch.clone();
            let delete_dispatch = dispatch.clone();
            let row_hook = hook.clone();
            let edit = Rc::clone(&edit_row);
            let scope_label = match hook.scope {
                HookScope::User => "用户",
                HookScope::Project => "项目",
            };
            let edit_button = Button::new(format!("settings-hook-edit:{hook_id}"), "编辑")
                .variant(ButtonVariant::Outline)
                .aria_label(format!(
                    "编辑{} Hook",
                    match hook.scope {
                        HookScope::User => "用户",
                        HookScope::Project => "项目",
                    }
                ))
                .disabled(!hook.editable)
                .on_click(move |event, _, cx| edit(&row_hook, event, cx));

            // 用户和项目 Hook 都要绑定当前 workspace 的摘要后才能信任或撤销。
            let admission = hook
                .workspace_identity
                .clone()
                .zip(hook.bundle_digest.clone())
                .zip(hook.hook_declaration_digest.clone());
            let trust_button = admission
                .filter(|_| hook.editable && hook.source != HookSource::Plugin)
                .and_then(|((workspace_identity, bundle_digest), declaration)| {
                    let (label, command) = match hook.trust_state {
                        HookTrustState::PendingTrust => (
                            "信任",
                            SettingsCommand::GrantHookTrust {
                                workspace_identity,
                                bundle_digest,
                                hook_declaration_digest: declaration,
                            },
                        ),
                        HookTrustState::TrustedPersistent => (
                            "撤销信任",
                            SettingsCommand::RevokeHookTrust {
                                workspace_identity,
                                bundle_digest,
                                hook_declaration_digest: declaration,
                            },
                        ),
                        HookTrustState::NotApplicable | HookTrustState::TrustStoreCorrupt => {
                            return None;
                        }
                    };
                    Some(
                        Button::new(format!("settings-hook-trust:{trust_id}"), label)
                            .variant(ButtonVariant::Outline)
                            .aria_label(format!("{label}{scope_label} Hook"))
                            .on_click(move |_, window, cx| {
                                trust_dispatch(command.clone(), window, cx)
                            }),
                    )
                });
            let toggle = Switch::new(format!("settings-hook-enabled:{hook_id}"), hook.enabled)
                .style(settings_switch_style(cx.theme()))
                .aria_label(format!("启用{scope_label} Hook"))
                .disabled(
                    !hook.editable
                        || hook.trust_state == HookTrustState::PendingTrust
                        || hook.trust_state == HookTrustState::TrustStoreCorrupt,
                )
                .on_change(move |enabled, window, cx| {
                    toggle_dispatch(
                        SettingsCommand::SetHookEnabled {
                            hook_id: hook_id.clone(),
                            scope: hook_scope,
                            enabled,
                        },
                        window,
                        cx,
                    )
                });
            let delete_button = Button::new(format!("settings-hook-delete:{delete_id}"), "删除")
                .variant(ButtonVariant::Danger)
                .aria_label(format!("删除{scope_label} Hook"))
                .disabled(!hook.editable)
                .on_click(move |_, window, cx| {
                    delete_dispatch(
                        SettingsCommand::DeleteHook {
                            hook_id: delete_id.clone(),
                            scope: hook_scope,
                        },
                        window,
                        cx,
                    )
                });
            SettingsRow::new(format!(
                "{} · {} · {}",
                hook.event.as_str(),
                hook_scope_name(hook.scope),
                hook_source_name(hook.source)
            ))
            .description(format!(
                "{}{}{}",
                hook_display_command(hook),
                hook.plugin_name
                    .as_deref()
                    .map(|name| format!(" · 插件 {name}"))
                    .unwrap_or_default(),
                hook.read_only_reason
                    .as_deref()
                    .map(|reason| format!(" · {reason}"))
                    .unwrap_or_default()
            ))
            .control(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(trust_button)
                    .child(edit_button)
                    .child(toggle)
                    .child(delete_button),
            )
        })
        .collect::<Vec<_>>();

    let plugin_rows = current.plugin_hooks.iter().map(|hook| {
        SettingsRow::new(format!(
            "{} · {} · {}",
            hook.plugin_name, hook.scope, hook.event
        ))
        .description(format!(
            "{}{}",
            hook.command,
            hook.reason
                .as_deref()
                .map(|reason| format!(" · {reason}"))
                .unwrap_or_default()
        ))
        .control(div().child(if hook.supported {
            "已支持"
        } else {
            "只读诊断"
        }))
    });

    let scope_select = hook_select(
        "settings-hook-scope-select",
        "Hook 作用域",
        fields.scope.clone(),
        [Choice::new("user", "用户"), Choice::new("project", "项目")],
        cx,
    );
    let event_select = hook_select(
        "settings-hook-event-select",
        "Hook 事件",
        fields.event.clone(),
        HookEvent::ALL.map(|event| Choice::new(event.as_str(), event.as_str())),
        cx,
    );
    let type_select = hook_select(
        "settings-hook-type-select",
        "Hook 类型",
        fields.hook_type.clone(),
        [
            Choice::new("command", "Command（命令）"),
            Choice::new("context", "Context（上下文）"),
        ],
        cx,
    );

    let form_description = form_error
        .read(cx)
        .as_deref()
        .map(|error| {
            format!("用户 Hook 和当前项目 Hook 由 NativeHost 校验并原子保存。保存失败：{error}")
        })
        .unwrap_or_else(|| "用户 Hook 和当前项目 Hook 由 NativeHost 校验并原子保存。".to_owned());
    let form = SettingsSection::new("Hook 配置")
        .description(form_description)
        .row(SettingsRow::new("作用域").control(div().w(px(300.)).child(scope_select)))
        .row(SettingsRow::new("事件").control(div().w(px(300.)).child(event_select)))
        .row(SettingsRow::new("类型").control(div().w(px(300.)).child(type_select)))
        .row(
            SettingsRow::new("匹配器")
                .control(div().w(px(300.)).child(Input::new(&fields.matcher))),
        )
        .row(
            SettingsRow::new("命令").control(
                div()
                    .w(px(520.))
                    .max_w_full()
                    .child(Input::new(&fields.command)),
            ),
        )
        .row(
            SettingsRow::new("参数").wide().control(
                div()
                    .w(px(520.))
                    .max_w_full()
                    .child(Input::new(&fields.args)),
            ),
        )
        .row(SettingsRow::new("Shell").control(div().w(px(300.)).child(Input::new(&fields.shell))))
        .row(
            SettingsRow::new("超时毫秒")
                .control(div().w(px(180.)).child(Input::new(&fields.timeout_ms))),
        )
        .row(
            SettingsRow::new("追加上下文").wide().control(
                div()
                    .w(px(520.))
                    .max_w_full()
                    .child(Input::new(&fields.context)),
            ),
        )
        .row(
            SettingsRow::new("阻断消息").wide().control(
                div()
                    .w(px(520.))
                    .max_w_full()
                    .child(Input::new(&fields.block)),
            ),
        )
        .row(
            SettingsRow::new("输入 JSON").wide().control(
                div()
                    .w(px(520.))
                    .max_w_full()
                    .child(Input::new(&fields.input)),
            ),
        )
        .row(
            SettingsRow::new("允许继续").control(
                Switch::new("settings-hook-continue", fields.continue_state.get())
                    .style(settings_switch_style(cx.theme()))
                    .on_change({
                        let continue_state = fields.continue_state.clone();
                        move |value, _, _| continue_state.set(value)
                    }),
            ),
        )
        .row(
            SettingsRow::new("操作").control(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("settings-hook-save", "保存")
                            .variant(ButtonVariant::Primary)
                            .on_click(save),
                    )
                    .child(
                        Button::new("settings-hook-new", "新建")
                            .variant(ButtonVariant::Outline)
                            .on_click(clear_form),
                    ),
            ),
        );

    let list = SettingsSection::new("用户与项目 Hook")
        .description(if current.trust_store_corrupt {
            "Trust 记录损坏，用户和项目 Hook 将保持禁用，修复记录后才能重新信任。"
        } else {
            "用户和项目 Hook 的执行摘要按当前项目授权；变更后需要重新信任。"
        })
        .rows_into(rows);
    let list = list.into_any_element();
    let plugins = SettingsSection::new("插件 Hook 诊断")
        .description("插件 Hook 随插件快照读取，只读展示，不允许通过此页修改。")
        .rows_into(plugin_rows.collect());

    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_5()
        .child(form)
        .child(list)
        .child(plugins)
        .into_any_element()
}

pub(super) fn render(
    current: &HooksSettings,
    dispatch: SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    render_form(current, &dispatch, window, cx)
}

trait SettingsSectionRowsExt {
    fn rows_into(self, rows: Vec<SettingsRow>) -> Self;
}

impl SettingsSectionRowsExt for SettingsSection {
    fn rows_into(self, rows: Vec<SettingsRow>) -> Self {
        rows.into_iter().fold(self, |section, row| section.row(row))
    }
}
