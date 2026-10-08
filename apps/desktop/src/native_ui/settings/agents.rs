use super::{
    SettingsCommandHandler, SettingsRow, SettingsSection, contracts::*,
    section::settings_switch_style,
};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{Input, Switch, TextInput},
    theme::ActiveTheme,
};
use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, Window, div, px};

pub(super) fn render(
    current: &AgentSettings,
    dispatch: SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let goal_revision = current.goal_revision;
    let goal_status = current
        .goal
        .as_ref()
        .map(|goal| goal.status.as_str())
        .unwrap_or("");
    let goal_is_active = goal_status == "active";
    let goal_is_paused = goal_status == "paused";
    let goal_is_terminal = matches!(goal_status, "completed" | "blocked");
    let goal_run_label = if goal_is_paused {
        "恢复目标"
    } else {
        "运行目标"
    };
    let active_root_turn_id = current
        .goal
        .as_ref()
        .and_then(|goal| goal.active_root_turn_id.clone());
    let has_active_root_turn = active_root_turn_id.is_some();
    let goal_title_value = current
        .goal
        .as_ref()
        .map(|goal| goal.title.clone())
        .unwrap_or_default();
    let goal_detail_value = current
        .goal
        .as_ref()
        .and_then(|goal| goal.detail.clone())
        .unwrap_or_default();
    let goal_title = window.use_keyed_state(
        format!("settings-goal-title:{goal_revision}"),
        cx,
        move |window, cx| {
            let mut field = TextInput::new(window, cx).placeholder("当前目标");
            field.set_text(goal_title_value, cx);
            field
        },
    );
    let goal_detail = window.use_keyed_state(
        format!("settings-goal-detail:{goal_revision}"),
        cx,
        move |window, cx| {
            let mut field = TextInput::new(window, cx)
                .multi_line(2, 6)
                .placeholder("目标说明");
            field.set_text(goal_detail_value, cx);
            field
        },
    );
    let goal_evidence = window.use_keyed_state(
        format!("settings-goal-evidence:{goal_revision}"),
        cx,
        move |window, cx| {
            TextInput::new(window, cx)
                .multi_line(2, 6)
                .placeholder("完成证据（必填）")
        },
    );
    let goal_block_reason = window.use_keyed_state(
        format!("settings-goal-block-reason:{goal_revision}"),
        cx,
        move |window, cx| {
            TextInput::new(window, cx)
                .multi_line(2, 6)
                .placeholder("阻塞原因（必填）")
        },
    );
    let set_goal_dispatch = dispatch.clone();
    let goal_title_field = goal_title.clone();
    let goal_detail_field = goal_detail.clone();
    let expected_revision = goal_revision;
    let goal_id = current
        .goal
        .as_ref()
        .map(|goal| goal.goal_id.clone())
        .unwrap_or_default();
    let set_goal = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let title = goal_title_field.read(cx).text().trim().to_owned();
        if title.is_empty() {
            return;
        }
        set_goal_dispatch(
            SettingsCommand::SetGoal {
                goal_id: goal_id.clone(),
                title,
                detail: Some(goal_detail_field.read(cx).text().to_owned()),
                expected_revision,
            },
            window,
            cx,
        );
    };
    let resume_goal_dispatch = dispatch.clone();
    let resume_goal_id = current
        .goal
        .as_ref()
        .map(|goal| goal.goal_id.clone())
        .unwrap_or_default();
    let resume_goal = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        if resume_goal_id.is_empty() {
            return;
        }
        resume_goal_dispatch(
            SettingsCommand::ResumeGoal {
                goal_id: resume_goal_id.clone(),
                expected_revision,
            },
            window,
            cx,
        );
    };
    let pause_goal_dispatch = dispatch.clone();
    let pause_goal_id = current
        .goal
        .as_ref()
        .map(|goal| goal.goal_id.clone())
        .unwrap_or_default();
    let pause_goal = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        if pause_goal_id.is_empty() {
            return;
        }
        pause_goal_dispatch(
            SettingsCommand::PauseGoal {
                goal_id: pause_goal_id.clone(),
                expected_revision,
            },
            window,
            cx,
        );
    };
    let cancel_goal_dispatch = dispatch.clone();
    let cancel_goal_id = current
        .goal
        .as_ref()
        .map(|goal| goal.goal_id.clone())
        .unwrap_or_default();
    let cancel_goal_turn_id = active_root_turn_id.clone().unwrap_or_default();
    let cancel_goal = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        if cancel_goal_id.is_empty() || cancel_goal_turn_id.is_empty() {
            return;
        }
        cancel_goal_dispatch(
            SettingsCommand::CancelGoalRun {
                goal_id: cancel_goal_id.clone(),
                expected_revision,
                turn_id: cancel_goal_turn_id.clone(),
            },
            window,
            cx,
        );
    };
    let complete_goal_dispatch = dispatch.clone();
    let complete_goal_id = current
        .goal
        .as_ref()
        .map(|goal| goal.goal_id.clone())
        .unwrap_or_default();
    let complete_goal_evidence = goal_evidence.clone();
    let complete_goal = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        if complete_goal_id.is_empty() {
            return;
        }
        let evidence = complete_goal_evidence.read(cx).text().trim().to_owned();
        if evidence.is_empty() {
            return;
        }
        complete_goal_dispatch(
            SettingsCommand::CompleteGoal {
                goal_id: complete_goal_id.clone(),
                expected_revision,
                evidence,
            },
            window,
            cx,
        );
    };
    let block_goal_dispatch = dispatch.clone();
    let block_goal_id = current
        .goal
        .as_ref()
        .map(|goal| goal.goal_id.clone())
        .unwrap_or_default();
    let block_goal_reason_field = goal_block_reason.clone();
    let block_goal = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        if block_goal_id.is_empty() {
            return;
        }
        let reason = block_goal_reason_field.read(cx).text().trim().to_owned();
        if reason.is_empty() {
            return;
        }
        block_goal_dispatch(
            SettingsCommand::BlockGoal {
                goal_id: block_goal_id.clone(),
                expected_revision,
                reason,
            },
            window,
            cx,
        );
    };
    let clear_goal_dispatch = dispatch.clone();
    let clear_goal_id = current
        .goal
        .as_ref()
        .map(|goal| goal.goal_id.clone())
        .unwrap_or_default();
    let clear_goal = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        if clear_goal_id.is_empty() {
            return;
        }
        clear_goal_dispatch(
            SettingsCommand::ClearGoal {
                goal_id: clear_goal_id.clone(),
                expected_revision,
            },
            window,
            cx,
        );
    };
    let subagent_section = current.subagents.iter().fold(
        SettingsSection::new("子智能体").description(
            "global/project 配置可启用、切换模型或删除；运行时子智能体仅显示状态摘要。",
        ),
        |section, agent| {
            // 只有文件来源能映射到可写目录时才绑定配置命令；runtime child 没有可编辑文件。
            let summary = agent
                .description
                .as_deref()
                .filter(|summary| !summary.trim().is_empty());
            let control = if matches!(agent.source.as_str(), "global" | "project") {
                let id = agent.id.clone();
                let id_for_delete = agent.id.clone();
                let id_for_model = agent.id.clone();
                let toggle = dispatch.clone();
                let delete = dispatch.clone();
                let model = dispatch.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Switch::new(format!("settings-subagent-enabled:{id}"), agent.enabled)
                            .style(settings_switch_style(cx.theme()))
                            .on_change(move |enabled, window, cx| {
                                toggle(
                                    SettingsCommand::SetSubagentEnabled {
                                        agent_id: id.clone(),
                                        enabled,
                                    },
                                    window,
                                    cx,
                                )
                            }),
                    )
                    .child(
                        Button::new(
                            format!("settings-subagent-model:{id_for_model}"),
                            "使用当前模型",
                        )
                        .variant(ButtonVariant::Outline)
                        .on_click(move |_, window, cx| {
                            model(
                                SettingsCommand::SetSubagentModel {
                                    agent_id: id_for_model.clone(),
                                    model: None,
                                },
                                window,
                                cx,
                            )
                        }),
                    )
                    .child(
                        Button::new(format!("settings-subagent-delete:{id_for_delete}"), "删除")
                            .variant(ButtonVariant::Danger)
                            .on_click(move |_, window, cx| {
                                delete(
                                    SettingsCommand::DeleteSubagent {
                                        agent_id: id_for_delete.clone(),
                                    },
                                    window,
                                    cx,
                                )
                            }),
                    )
            } else {
                let status = if agent.enabled { "活动" } else { "已停止" };
                let mut runtime = div().flex().flex_col().gap_1().child(
                    ely_gpui_component::typography::Caption::new(format!("运行状态：{status}")),
                );
                if let Some(summary) = summary {
                    runtime = runtime.child(ely_gpui_component::typography::Caption::new(format!(
                        "任务摘要：{summary}"
                    )));
                }
                runtime
            };
            section.row(
                SettingsRow::new(agent.name.clone())
                    .description(match summary {
                        Some(summary) => {
                            format!("{} · {} 工具 · {summary}", agent.source, agent.tools.len())
                        }
                        None => format!("{} · {} 工具", agent.source, agent.tools.len()),
                    })
                    .control(control),
            )
        },
    );
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_5()
        .child(
            SettingsSection::new("当前目标")
                .description(
                    current
                        .goal
                        .as_ref()
                        .map(|goal| format!("{} · {}", goal.status, goal.title))
                        .unwrap_or_else(|| "尚未设置目标".to_owned()),
                )
                .row(
                    SettingsRow::new("目标标题").wide().control(
                        div()
                            .w(px(360.))
                            .max_w_full()
                            .child(Input::new(&goal_title)),
                    ),
                )
                .row(
                    SettingsRow::new("目标说明").wide().control(
                        div()
                            .w(px(460.))
                            .max_w_full()
                            .child(Input::new(&goal_detail)),
                    ),
                )
                .row(
                    SettingsRow::new("完成证据").wide().control(
                        div()
                            .w(px(460.))
                            .max_w_full()
                            .child(Input::new(&goal_evidence)),
                    ),
                )
                .row(
                    SettingsRow::new("阻塞原因").wide().control(
                        div()
                            .w(px(460.))
                            .max_w_full()
                            .child(Input::new(&goal_block_reason)),
                    ),
                )
                .row(
                    SettingsRow::new("运行状态").description(if has_active_root_turn {
                        "任务正在运行"
                    } else {
                        "当前没有运行中的任务"
                    }),
                )
                .row(
                    SettingsRow::new("操作").control(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new("settings-goal-set", "保存目标")
                                    .variant(ButtonVariant::Primary)
                                    .disabled(goal_is_terminal)
                                    .on_click(set_goal),
                            )
                            .child(
                                Button::new("settings-goal-resume", goal_run_label)
                                    .variant(ButtonVariant::Primary)
                                    .disabled(
                                        current.goal.is_none()
                                            || goal_is_terminal
                                            || has_active_root_turn,
                                    )
                                    .on_click(resume_goal),
                            )
                            .child(
                                Button::new("settings-goal-pause", "暂停目标")
                                    .variant(ButtonVariant::Outline)
                                    .disabled(!goal_is_active)
                                    .on_click(pause_goal),
                            )
                            .child(
                                Button::new("settings-goal-cancel", "取消本次运行")
                                    .variant(ButtonVariant::Outline)
                                    .disabled(!has_active_root_turn || goal_is_terminal)
                                    .on_click(cancel_goal),
                            )
                            .child(
                                Button::new("settings-goal-complete", "标记完成")
                                    .variant(ButtonVariant::Primary)
                                    .disabled(goal_is_terminal || has_active_root_turn)
                                    .on_click(complete_goal),
                            )
                            .child(
                                Button::new("settings-goal-block", "标记阻塞")
                                    .variant(ButtonVariant::Danger)
                                    .disabled(goal_is_terminal || has_active_root_turn)
                                    .on_click(block_goal),
                            )
                            .child(
                                Button::new("settings-goal-clear", "清除目标")
                                    .variant(ButtonVariant::Danger)
                                    .disabled(!goal_is_terminal)
                                    .on_click(clear_goal),
                            ),
                    ),
                ),
        )
        .child(subagent_section)
        .into_any_element()
}
