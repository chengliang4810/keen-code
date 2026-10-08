use super::{
    SettingsCommandHandler, SettingsRow, SettingsSection, contracts::*,
    section::settings_switch_style,
};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{Input, Switch, TextInput},
    theme::ActiveTheme,
    typography::Caption,
};
use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, Window, div, px};

pub(super) fn render(
    current: &AutomationSettings,
    dispatch: SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let title = window.use_keyed_state("settings-automation-title", cx, |window, cx| {
        TextInput::new(window, cx)
            .label("新建任务名称")
            .placeholder("任务名称")
    });
    let prompt = window.use_keyed_state("settings-automation-prompt", cx, |window, cx| {
        TextInput::new(window, cx)
            .label("新建任务提示词")
            .multi_line(3, 8)
            .placeholder("任务提示词")
    });
    let cron = window.use_keyed_state("settings-automation-cron", cx, |window, cx| {
        TextInput::new(window, cx)
            .label("新建任务 Cron")
            .placeholder("0 9 * * 1-5")
    });
    let create_dispatch = dispatch.clone();
    let title_field = title.clone();
    let prompt_field = prompt.clone();
    let cron_field = cron.clone();
    let create = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let title_value = title_field.read(cx).text().trim().to_owned();
        let prompt_value = prompt_field.read(cx).text().trim().to_owned();
        let cron_value = cron_field.read(cx).text().trim().to_owned();
        if title_value.is_empty() || prompt_value.is_empty() || cron_value.is_empty() {
            return;
        }
        create_dispatch(
            SettingsCommand::CreateAutomation(AutomationRecord {
                id: String::new(),
                title: title_value,
                prompt: prompt_value,
                cron_expr: cron_value,
                enabled: true,
                recurring: true,
                max_runs: None,
                next_run_at_ms: None,
                last_error: None,
                history: Vec::new(),
            }),
            window,
            cx,
        );
    };
    let section = current.items.iter().fold(
        SettingsSection::new("已保存任务")
            .description("任务会按计划运行；离开此页面不会影响已排队的任务。"),
        |section, automation| {
            let id = automation.id.clone();
            let id_for_toggle = id.clone();
            let id_for_run = automation.id.clone();
            let id_for_history = automation.id.clone();
            let id_for_delete = automation.id.clone();
            let active_run_id = automation
                .history
                .iter()
                .rev()
                .find(|run| run.status == "running")
                .map(|run| run.id.clone());
            let id_for_cancel = active_run_id.clone().unwrap_or_default();
            let title_editor = window.use_keyed_state(
                format!("settings-automation-edit-title:{}", automation.id),
                cx,
                {
                    let value = automation.title.clone();
                    move |window, cx| {
                        let mut field = TextInput::new(window, cx)
                            .label("编辑任务名称")
                            .placeholder("任务名称");
                        field.set_text(value, cx);
                        field
                    }
                },
            );
            let prompt_editor = window.use_keyed_state(
                format!("settings-automation-edit-prompt:{}", automation.id),
                cx,
                {
                    let value = automation.prompt.clone();
                    move |window, cx| {
                        let mut field = TextInput::new(window, cx)
                            .label("编辑任务提示词")
                            .multi_line(3, 8)
                            .placeholder("任务提示词");
                        field.set_text(value, cx);
                        field
                    }
                },
            );
            let cron_editor = window.use_keyed_state(
                format!("settings-automation-edit-cron:{}", automation.id),
                cx,
                {
                    let value = automation.cron_expr.clone();
                    move |window, cx| {
                        let mut field = TextInput::new(window, cx)
                            .label("编辑任务 Cron")
                            .placeholder("Cron 表达式");
                        field.set_text(value, cx);
                        field
                    }
                },
            );
            let toggle = dispatch.clone();
            let run = dispatch.clone();
            let history = dispatch.clone();
            let delete = dispatch.clone();
            let cancel = dispatch.clone();
            let update = dispatch.clone();
            let update_record = automation.clone();
            let update_title = title_editor.clone();
            let update_prompt = prompt_editor.clone();
            let update_cron = cron_editor.clone();
            let update_automation =
                move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                    let mut next = update_record.clone();
                    next.title = update_title.read(cx).text().trim().to_owned();
                    next.prompt = update_prompt.read(cx).text().to_owned();
                    next.cron_expr = update_cron.read(cx).text().trim().to_owned();
                    if next.title.is_empty()
                        || next.prompt.trim().is_empty()
                        || next.cron_expr.is_empty()
                    {
                        return;
                    }
                    update(SettingsCommand::UpdateAutomation(next), window, cx);
                };
            let status = if active_run_id.is_some() {
                format!("运行中 · {}", automation.cron_expr)
            } else if let Some(error) = &automation.last_error {
                format!("失败：{}", error)
            } else if automation.enabled {
                format!("已启用 · {}", automation.cron_expr)
            } else {
                format!("已暂停 · {}", automation.cron_expr)
            };
            section
                .row(
                    SettingsRow::new(automation.title.clone())
                        .description(status)
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Switch::new(
                                        format!("settings-automation-enabled:{id}"),
                                        automation.enabled,
                                    )
                                    .style(settings_switch_style(cx.theme()))
                                    .label("启用自动化")
                                    .on_change(
                                        move |enabled, window, cx| {
                                            toggle(
                                                SettingsCommand::SetAutomationEnabled {
                                                    automation_id: id_for_toggle.clone(),
                                                    enabled,
                                                },
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                )
                                .child(
                                    Button::new(
                                        format!("settings-automation-run:{id_for_run}"),
                                        "立即运行",
                                    )
                                    .variant(ButtonVariant::Primary)
                                    .disabled(active_run_id.is_some())
                                    .on_click(
                                        move |_, window, cx| {
                                            run(
                                                SettingsCommand::RunAutomation {
                                                    automation_id: id_for_run.clone(),
                                                },
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                )
                                .child(
                                    Button::new(
                                        format!("settings-automation-cancel:{id}"),
                                        "取消运行",
                                    )
                                    .variant(ButtonVariant::Danger)
                                    .disabled(active_run_id.is_none())
                                    .on_click(
                                        move |_, window, cx| {
                                            cancel(
                                                SettingsCommand::CancelAutomation {
                                                    run_id: id_for_cancel.clone(),
                                                },
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                )
                                .child(
                                    Button::new(
                                        format!("settings-automation-history:{id_for_history}"),
                                        "历史",
                                    )
                                    .variant(ButtonVariant::Outline)
                                    .on_click(
                                        move |_, window, cx| {
                                            history(
                                                SettingsCommand::ListAutomationHistory {
                                                    automation_id: id_for_history.clone(),
                                                },
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                )
                                .child(
                                    Button::new(
                                        format!("settings-automation-delete:{id_for_delete}"),
                                        "删除",
                                    )
                                    .variant(ButtonVariant::Danger)
                                    .on_click(
                                        move |_, window, cx| {
                                            delete(
                                                SettingsCommand::DeleteAutomation {
                                                    automation_id: id_for_delete.clone(),
                                                },
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                ),
                        ),
                )
                .row(
                    SettingsRow::new("编辑任务").control(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(220.))
                                    .max_w_full()
                                    .child(Input::new(&title_editor)),
                            )
                            .child(
                                div()
                                    .w(px(360.))
                                    .max_w_full()
                                    .child(Input::new(&prompt_editor)),
                            )
                            .child(
                                div()
                                    .w(px(180.))
                                    .max_w_full()
                                    .child(Input::new(&cron_editor)),
                            )
                            .child(
                                Button::new(format!("settings-automation-save:{id}"), "保存任务")
                                    .variant(ButtonVariant::Outline)
                                    .on_click(update_automation),
                            ),
                    ),
                )
                .row(
                    SettingsRow::new("运行历史").control(if automation.history.is_empty() {
                        Caption::new("暂无运行记录").into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(automation.history.iter().map(|run| {
                                Caption::new(format!(
                                    "{} · {}{}",
                                    run.status,
                                    run.id,
                                    run.detail
                                        .as_deref()
                                        .map(|detail| format!(" · {detail}"))
                                        .unwrap_or_default()
                                ))
                            }))
                            .into_any_element()
                    }),
                )
        },
    );
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_5()
        .child(
            SettingsSection::new("新建定时任务")
                .description("保存前会校验 Cron 表达式和工作区、模型设置。")
                .row(
                    SettingsRow::new("名称")
                        .control(div().w(px(300.)).max_w_full().child(Input::new(&title))),
                )
                .row(
                    SettingsRow::new("提示词")
                        .wide()
                        .control(div().w(px(420.)).max_w_full().child(Input::new(&prompt))),
                )
                .row(
                    SettingsRow::new("Cron")
                        .control(div().w(px(260.)).max_w_full().child(Input::new(&cron))),
                )
                .row(
                    SettingsRow::new("操作").control(
                        Button::new("settings-automation-create", "创建任务")
                            .variant(ButtonVariant::Primary)
                            .on_click(create),
                    ),
                ),
        )
        .child(section)
        .into_any_element()
}
