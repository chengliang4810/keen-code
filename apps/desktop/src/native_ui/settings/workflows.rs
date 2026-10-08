use super::{SettingsCommandHandler, SettingsRow, SettingsSection, contracts::*};
use crate::workflows::DynamicWorkflowRunProgressPayload;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{Choice, Combobox, Input, TextInput},
    typography::Caption,
};
use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, Window, div, px};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_DISPLAY_TEXT_CHARS: usize = 4_000;

pub(super) fn render(
    current: &WorkflowSettings,
    dispatch: SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let new_name = window.use_keyed_state("settings-workflow-new-name", cx, |window, cx| {
        TextInput::new(window, cx).placeholder("工作流名称")
    });
    let new_scope = window.use_keyed_state("settings-workflow-new-scope", cx, |window, cx| {
        let mut field = TextInput::new(window, cx).placeholder("global / project");
        field.set_text("global", cx);
        field
    });
    // Combobox 显示的是 label；协议作用域单独保存，避免渲染时的显示文本覆盖提交值。
    let new_scope_value =
        window.use_keyed_state("settings-workflow-new-scope-value", cx, |_, _| {
            "global".to_owned()
        });
    let new_description =
        window.use_keyed_state("settings-workflow-new-description", cx, |window, cx| {
            TextInput::new(window, cx).placeholder("说明（可选）")
        });
    let new_when_to_use =
        window.use_keyed_state("settings-workflow-new-when-to-use", cx, |window, cx| {
            TextInput::new(window, cx).placeholder("何时使用（可选）")
        });
    let new_tags = window.use_keyed_state("settings-workflow-new-tags", cx, |window, cx| {
        TextInput::new(window, cx).placeholder("标签，用逗号分隔（可选）")
    });
    let new_definition =
        window.use_keyed_state("settings-workflow-new-definition", cx, |window, cx| {
            TextInput::new(window, cx)
                .multi_line(8, 24)
                .placeholder("WorkflowDefinitionV1 JSON")
        });
    let create_dispatch = dispatch.clone();
    let create_name = new_name.clone();
    let create_scope_value = new_scope_value.clone();
    let create_description = new_description.clone();
    let create_when_to_use = new_when_to_use.clone();
    let create_tags = new_tags.clone();
    let create_definition = new_definition.clone();
    let save_new = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let name = create_name.read(cx).text().trim().to_owned();
        let scope_value = create_scope_value.read(cx).clone();
        let definition = create_definition.read(cx).text().trim().to_owned();
        let Some(scope) = workflow_scope_value(&scope_value).map(str::to_owned) else {
            log::warn!("拒绝保存工作流：作用域值无效");
            return;
        };
        if let Some(reason) = validate_new_workflow(&name, &scope, &definition) {
            log::warn!("拒绝保存工作流：{reason}");
            return;
        }
        let (definition_description, definition_when_to_use, definition_tags) =
            definition_metadata(&definition);
        create_dispatch(
            SettingsCommand::SaveWorkflow(WorkflowRecord {
                name,
                scope,
                path: String::new(),
                revision: 0,
                // 新建表单的留空值沿用 JSON 中已有 meta，避免手工编辑合法定义时
                // 因设置页的辅助字段为空而静默丢失 metadata。
                description: optional_text(create_description.read(cx).text())
                    .or(definition_description),
                when_to_use: optional_text(create_when_to_use.read(cx).text())
                    .or(definition_when_to_use),
                tags: non_empty_tags(parse_tags(create_tags.read(cx).text()))
                    .unwrap_or(definition_tags),
                definition_json: definition,
                valid: false,
                validation_error: None,
                runs: Vec::new(),
            }),
            window,
            cx,
        );
    };

    let inspection_data = current.inspection.as_ref();
    let workflow_sections = current
        .items
        .iter()
        .map(|workflow| {
            let initial_definition = workflow.definition_json.clone();
            let editor = window.use_keyed_state(
                format!(
                    "settings-workflow-editor:{}:{}:{}",
                    workflow.scope, workflow.name, workflow.revision
                ),
                cx,
                move |window, cx| {
                    let mut field = TextInput::new(window, cx)
                        .multi_line(8, 24)
                        .placeholder("WorkflowDefinitionV1 JSON");
                    field.set_text(initial_definition.clone(), cx);
                    field
                },
            );
            let description_editor = workflow_text_field(
                window,
                cx,
                format!(
                    "settings-workflow-description:{}:{}:{}",
                    workflow.scope, workflow.name, workflow.revision
                ),
                "说明（可选）",
                workflow.description.clone().unwrap_or_default(),
            );
            let when_to_use_editor = workflow_text_field(
                window,
                cx,
                format!(
                    "settings-workflow-when-to-use:{}:{}:{}",
                    workflow.scope, workflow.name, workflow.revision
                ),
                "何时使用（可选）",
                workflow.when_to_use.clone().unwrap_or_default(),
            );
            let tags_editor = workflow_text_field(
                window,
                cx,
                format!(
                    "settings-workflow-tags:{}:{}:{}",
                    workflow.scope, workflow.name, workflow.revision
                ),
                "标签，用逗号分隔（可选）",
                workflow.tags.join(", "),
            );

            let name = workflow.name.clone();
            let scope = workflow.scope.clone();
            let name_for_run = name.clone();
            let scope_for_run = scope.clone();
            let name_for_runs = name.clone();
            let scope_for_runs = scope.clone();
            let name_for_delete = name.clone();
            let scope_for_delete = scope.clone();
            let name_for_move = name.clone();
            let scope_for_move = scope.clone();
            let name_for_save = name.clone();
            let scope_for_save = scope.clone();
            let revision = workflow.revision;
            let path = workflow.path.clone();
            let valid = workflow.valid;
            let validation_error = workflow.validation_error.clone();
            let definition = editor.clone();
            let description = description_editor.clone();
            let when_to_use = when_to_use_editor.clone();
            let tags = tags_editor.clone();
            let save = dispatch.clone();
            let run = dispatch.clone();
            let runs = dispatch.clone();
            let move_workflow = dispatch.clone();
            let delete = dispatch.clone();
            let save_button_id = format!("settings-workflow-save:{}:{}", scope, name);
            let move_button_id = format!("settings-workflow-move:{}:{}", scope, name);
            let status = if workflow.valid {
                format!(
                    "修订 {} · 可执行 · {} 次运行",
                    workflow.revision,
                    workflow.runs.len()
                )
            } else {
                format!(
                    "修订 {} · 校验失败：{}",
                    workflow.revision,
                    workflow.validation_error.as_deref().unwrap_or("未知错误")
                )
            };
            let target_scope = if scope == "global" {
                "project"
            } else {
                "global"
            };
            let run_rows = workflow.runs.iter().map(|run| {
                Caption::new(format!(
                    "{} · {} · 事件 {} · 产物 {} · {}",
                    run.status,
                    run.run_id,
                    run.event_count,
                    run.artifact_count,
                    run.error.as_deref().unwrap_or("无错误")
                ))
                .into_any_element()
            });

            SettingsSection::new(format!("{} / {}", workflow.scope, workflow.name))
                .description(status)
                .row(
                    SettingsRow::new("定义与元数据")
                        .description("保存时会重新校验定义；当前版本用于检查并发修改。")
                        .control(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .w(px(560.))
                                .max_w_full()
                                .child(Input::new(&description))
                                .child(Input::new(&when_to_use))
                                .child(Input::new(&tags))
                                .child(Input::new(&editor))
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_2()
                                        .child(
                                            Button::new(save_button_id, "保存修订")
                                                .variant(ButtonVariant::Primary)
                                                .on_click(
                                                    move |_: &gpui::ClickEvent, window, cx| {
                                                        save(
                                                            SettingsCommand::SaveWorkflow(
                                                                WorkflowRecord {
                                                                    name: name_for_save.clone(),
                                                                    scope: scope_for_save.clone(),
                                                                    path: path.clone(),
                                                                    revision,
                                                                    description: optional_text(
                                                                        description.read(cx).text(),
                                                                    ),
                                                                    when_to_use: optional_text(
                                                                        when_to_use.read(cx).text(),
                                                                    ),
                                                                    tags: parse_tags(
                                                                        tags.read(cx).text(),
                                                                    ),
                                                                    definition_json: definition
                                                                        .read(cx)
                                                                        .text()
                                                                        .to_owned(),
                                                                    valid,
                                                                    validation_error:
                                                                        validation_error.clone(),
                                                                    runs: Vec::new(),
                                                                },
                                                            ),
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                ),
                                        )
                                        .child(
                                            Button::new(
                                                move_button_id,
                                                if target_scope == "project" {
                                                    "移动到项目"
                                                } else {
                                                    "移动到全局"
                                                },
                                            )
                                            .variant(ButtonVariant::Outline)
                                            .on_click(
                                                move |_, window, cx| {
                                                    move_workflow(
                                                        SettingsCommand::MoveWorkflow {
                                                            name: name_for_move.clone(),
                                                            from_scope: scope_for_move.clone(),
                                                            to_scope: target_scope.to_owned(),
                                                            expected_revision: revision,
                                                        },
                                                        window,
                                                        cx,
                                                    )
                                                },
                                            ),
                                        )
                                        .child(
                                            Button::new(
                                                format!(
                                                    "settings-workflow-run:{}:{}",
                                                    scope_for_run, name_for_run
                                                ),
                                                "执行",
                                            )
                                            .variant(ButtonVariant::Outline)
                                            .disabled(!workflow.valid)
                                            .on_click(
                                                move |_, window, cx| {
                                                    run(
                                                        SettingsCommand::RunWorkflow {
                                                            name: name_for_run.clone(),
                                                            scope: scope_for_run.clone(),
                                                            inputs_json: "{}".to_owned(),
                                                        },
                                                        window,
                                                        cx,
                                                    )
                                                },
                                            ),
                                        )
                                        .child(
                                            Button::new(
                                                format!(
                                                    "settings-workflow-runs:{}:{}",
                                                    scope_for_runs, name_for_runs
                                                ),
                                                "运行历史",
                                            )
                                            .variant(ButtonVariant::Outline)
                                            .on_click(
                                                move |_, window, cx| {
                                                    runs(
                                                        SettingsCommand::ListWorkflowRuns {
                                                            name: name_for_runs.clone(),
                                                            scope: scope_for_runs.clone(),
                                                        },
                                                        window,
                                                        cx,
                                                    )
                                                },
                                            ),
                                        )
                                        .child(
                                            Button::new(
                                                format!(
                                                    "settings-workflow-delete:{}:{}",
                                                    scope_for_delete, name_for_delete
                                                ),
                                                "删除",
                                            )
                                            .variant(ButtonVariant::Danger)
                                            .on_click(
                                                move |_, window, cx| {
                                                    delete(
                                                        SettingsCommand::DeleteWorkflow {
                                                            name: name_for_delete.clone(),
                                                            scope: scope_for_delete.clone(),
                                                        },
                                                        window,
                                                        cx,
                                                    )
                                                },
                                            ),
                                        ),
                                ),
                        ),
                )
                .row(
                    SettingsRow::new("运行查询")
                        .description("显示运行事件、文本和 JSON 结果；过长内容会按页面上限截断。")
                        .control(workflow_query_controls(
                            &workflow.runs,
                            inspection_data,
                            editor.clone(),
                            dispatch.clone(),
                        )),
                )
                .row(
                    SettingsRow::new("运行历史").control(if workflow.runs.is_empty() {
                        Caption::new("暂无运行记录").into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(run_rows)
                            .into_any_element()
                    }),
                )
                .into_any_element()
        })
        .collect::<Vec<_>>();

    let inspection =
        inspection_data.map(|value| render_inspection(value, dispatch.clone(), window, cx));
    let scope_value_picker = new_scope_value.clone();
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_5()
        .children(inspection)
        .child(
            SettingsSection::new("新建工作流")
                .description("global 保存到用户数据根；project 使用当前授权项目。")
                .row(
                    SettingsRow::new("名称")
                        .control(div().w(px(300.)).max_w_full().child(Input::new(&new_name))),
                )
                .row(
                    SettingsRow::new("作用域").control(
                        div().w(px(240.)).max_w_full().child(
                            Combobox::new(
                                "settings-workflow-new-scope-picker",
                                &new_scope,
                                [
                                    Choice::new("global", "全局"),
                                    Choice::new("project", "项目"),
                                ],
                            )
                            .selected(new_scope_value.read(cx).clone())
                            .on_change(move |value, _window, cx| {
                                let value =
                                    workflow_scope_value(value.as_str()).unwrap_or_default();
                                scope_value_picker.update(cx, |scope, cx| {
                                    *scope = value.to_owned();
                                    cx.notify();
                                });
                            }),
                        ),
                    ),
                )
                .row(
                    SettingsRow::new("说明").wide().control(
                        div()
                            .w(px(420.))
                            .max_w_full()
                            .child(Input::new(&new_description)),
                    ),
                )
                .row(
                    SettingsRow::new("何时使用").wide().control(
                        div()
                            .w(px(420.))
                            .max_w_full()
                            .child(Input::new(&new_when_to_use)),
                    ),
                )
                .row(
                    SettingsRow::new("标签")
                        .wide()
                        .control(div().w(px(420.)).max_w_full().child(Input::new(&new_tags))),
                )
                .row(
                    SettingsRow::new("定义").wide().control(
                        div()
                            .w(px(560.))
                            .max_w_full()
                            .child(Input::new(&new_definition)),
                    ),
                )
                .row(
                    SettingsRow::new("操作").control(
                        Button::new("settings-workflow-create", "保存工作流")
                            .variant(ButtonVariant::Primary)
                            .on_click(save_new),
                    ),
                ),
        )
        .children(workflow_sections)
        .into_any_element()
}

fn workflow_query_controls(
    runs: &[WorkflowRun],
    inspection: Option<&WorkflowInspection>,
    draft: gpui::Entity<TextInput>,
    dispatch: SettingsCommandHandler,
) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .gap_2()
        .children(runs.iter().map(|run| {
            let run_id = run.run_id.clone();
            let run_id_for_graph = run_id.clone();
            let run_id_for_workspace = run_id.clone();
            let run_id_for_artifacts = run_id.clone();
            let run_id_for_resume = run_id.clone();
            let run_id_for_amend = run_id.clone();
            let events = dispatch.clone();
            let graph = dispatch.clone();
            let workspace = dispatch.clone();
            let artifacts = dispatch.clone();
            let resume = dispatch.clone();
            let amend = dispatch.clone();
            let cancel = dispatch.clone();
            let cancel_id = run_id.clone();
            let draft_for_amend = draft.clone();
            let action_state = workflow_run_action_state(run, inspection);
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .child(Caption::new(format!("{} · {}", run.status, run_id)))
                .child(
                    Button::new(format!("settings-workflow-events:{run_id}"), "事件")
                        .variant(ButtonVariant::Ghost)
                        .on_click(move |_, window, cx| {
                            events(
                                SettingsCommand::ReadWorkflowEvents {
                                    run_id: run_id.clone(),
                                },
                                window,
                                cx,
                            )
                        }),
                )
                .child(
                    Button::new(format!("settings-workflow-graph:{run_id_for_graph}"), "图")
                        .variant(ButtonVariant::Ghost)
                        .on_click(move |_, window, cx| {
                            graph(
                                SettingsCommand::ReadWorkflowGraph {
                                    run_id: run_id_for_graph.clone(),
                                },
                                window,
                                cx,
                            )
                        }),
                )
                .child(
                    Button::new(
                        format!("settings-workflow-workspace:{run_id_for_workspace}"),
                        "工作区节点",
                    )
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, window, cx| {
                        workspace(
                            SettingsCommand::ReadWorkflowWorkspace {
                                run_id: run_id_for_workspace.clone(),
                            },
                            window,
                            cx,
                        )
                    }),
                )
                .child(
                    Button::new(
                        format!("settings-workflow-artifacts:{run_id_for_artifacts}"),
                        "产物元数据",
                    )
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, window, cx| {
                        artifacts(
                            SettingsCommand::ListWorkflowArtifacts {
                                run_id: run_id_for_artifacts.clone(),
                            },
                            window,
                            cx,
                        )
                    }),
                )
                .child(
                    Button::new(
                        format!("settings-workflow-resume:{run_id_for_resume}"),
                        "恢复运行",
                    )
                    .variant(ButtonVariant::Outline)
                    .disabled(!action_state.resumable)
                    .on_click(move |_, window, cx| {
                        resume(
                            SettingsCommand::ResumeWorkflow {
                                run_id: run_id_for_resume.clone(),
                            },
                            window,
                            cx,
                        )
                    }),
                )
                .child(
                    Button::new(
                        format!("settings-workflow-amend:{run_id_for_amend}"),
                        "修订运行",
                    )
                    .variant(ButtonVariant::Outline)
                    .disabled(!action_state.amendable)
                    .on_click(move |_, window, cx| {
                        let definition_json = draft_for_amend.read(cx).text().trim().to_owned();
                        if definition_json.is_empty() {
                            return;
                        }
                        amend(
                            SettingsCommand::AmendWorkflow {
                                predecessor_run_id: run_id_for_amend.clone(),
                                definition_json,
                                inputs_json: "{}".to_owned(),
                            },
                            window,
                            cx,
                        )
                    }),
                )
                .child(
                    Button::new(format!("settings-workflow-cancel:{cancel_id}"), "取消运行")
                        .variant(ButtonVariant::Danger)
                        .disabled(!action_state.active)
                        .on_click(move |_, window, cx| {
                            cancel(
                                SettingsCommand::CancelWorkflow {
                                    run_id: cancel_id.clone(),
                                },
                                window,
                                cx,
                            )
                        }),
                )
        }))
        .into_any_element()
}

#[derive(Clone, Copy)]
struct WorkflowRunActionState {
    active: bool,
    amendable: bool,
    resumable: bool,
}

fn workflow_run_action_state(
    run: &WorkflowRun,
    inspection: Option<&WorkflowInspection>,
) -> WorkflowRunActionState {
    let inspected = inspection
        .filter(|value| value.run_id == run.run_id)
        .and_then(|value| value.run.as_ref());
    let status = inspected
        .map(|value| value.status.as_str())
        .unwrap_or(run.status.as_str());
    let active_barrier = inspection
        .filter(|value| value.run_id == run.run_id)
        .and_then(|value| value.active_barrier.as_ref())
        .map(|value| value.active);
    let active = matches!(status, "running" | "pending") && active_barrier.unwrap_or(true);
    // 修订只能从仍可能产生事实的运行，或 Host 已明确标记为 superseded 的运行创建 successor。
    let amendable = (matches!(status, "running" | "pending") && active_barrier.unwrap_or(true))
        || inspected.is_some_and(|value| {
            status == "stopped" && value.stop_reason.as_deref() == Some("superseded")
        });

    // Resume 只有 Host 明确给出 resumable 且运行已离开活动屏障时才开放，避免用终态按钮推断可恢复性。
    WorkflowRunActionState {
        active,
        amendable,
        resumable: !active && inspected.is_some_and(|value| value.resumable),
    }
}

fn workflow_text_field(
    window: &mut Window,
    cx: &mut App,
    key: String,
    placeholder: &'static str,
    value: String,
) -> gpui::Entity<TextInput> {
    window.use_keyed_state(key, cx, move |window, cx| {
        let mut field = TextInput::new(window, cx).placeholder(placeholder);
        field.set_text(value.clone(), cx);
        field
    })
}

/// 设置页只从 Host 返回的进度事件归约待决问题，qid 是回答命令的唯一身份。
struct WorkflowPendingQuestion {
    id: String,
    question: String,
    context: Option<String>,
    actor: Option<String>,
}

fn pending_workflow_questions(
    events: &[DynamicWorkflowRunProgressPayload],
) -> Vec<WorkflowPendingQuestion> {
    let mut pending = BTreeMap::<String, WorkflowPendingQuestion>::new();
    for event in events {
        match event.event_type.as_str() {
            "escalation-raised" | "question-raised" | "question-asked" => {
                let Some(id) = workflow_question_field(
                    &event.payload,
                    &[
                        "qid",
                        "questionId",
                        "question_id",
                        "requestId",
                        "request_id",
                    ],
                ) else {
                    continue;
                };
                let Some(question) = workflow_question_field(
                    &event.payload,
                    &["question", "prompt", "text", "message"],
                ) else {
                    continue;
                };
                let context = workflow_question_field(&event.payload, &["context"]);
                let actor = workflow_question_actor(&event.payload);
                pending.insert(
                    id.clone(),
                    WorkflowPendingQuestion {
                        id,
                        question,
                        context,
                        actor,
                    },
                );
            }
            "escalation-resolved" | "question-resolved" | "question-answered" => {
                if let Some(id) = workflow_question_field(
                    &event.payload,
                    &[
                        "qid",
                        "questionId",
                        "question_id",
                        "requestId",
                        "request_id",
                    ],
                ) {
                    pending.remove(&id);
                }
            }
            _ => {}
        }
    }
    pending.into_values().collect()
}

fn workflow_question_field(payload: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        payload
            .get(*name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn workflow_question_actor(payload: &Value) -> Option<String> {
    workflow_question_field(payload, &["actorName", "actor_name"]).or_else(|| {
        let actor = payload.get("actor").and_then(Value::as_object)?;
        let site_id = actor
            .get("siteId")
            .or_else(|| actor.get("site_id"))
            .and_then(Value::as_str)?;
        let ordinal = actor
            .get("ordinal")
            .and_then(Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".to_owned());
        Some(format!("{site_id}@{ordinal}"))
    })
}

fn workflow_inspection_is_active(inspection: &WorkflowInspection) -> bool {
    inspection.run.as_ref().is_some_and(|run| {
        matches!(run.status.as_str(), "running" | "pending")
            && inspection
                .active_barrier
                .as_ref()
                .is_none_or(|barrier| barrier.active)
    })
}

fn render_workflow_question(
    inspection: &WorkflowInspection,
    question: WorkflowPendingQuestion,
    can_answer: bool,
    dispatch: SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let input = window.use_keyed_state(
        format!(
            "settings-workflow-question-answer:{}:{}",
            inspection.run_id, question.id
        ),
        cx,
        |window, cx| {
            TextInput::new(window, cx)
                .multi_line(3, 8)
                .placeholder("输入问题回答")
        },
    );
    let answer_is_empty = input.read(cx).text().trim().is_empty();
    let submit = dispatch;
    let submit_input = input.clone();
    let question_id = question.id.clone();
    let button = Button::new(
        format!(
            "settings-workflow-question-submit:{}:{}",
            inspection.run_id, question.id
        ),
        "提交回答",
    )
    .variant(ButtonVariant::Primary)
    .disabled(!can_answer || answer_is_empty)
    .on_click(move |_, window, cx| {
        let answer = submit_input.read(cx).text().trim().to_owned();
        if answer.is_empty() {
            return;
        }
        submit(
            SettingsCommand::ResolveWorkflowQuestion {
                question_id: question_id.clone(),
                answer,
            },
            window,
            cx,
        );
    });
    let mut label = format!("待决问题 · {}", question.question);
    if let Some(actor) = question.actor {
        label.push_str(&format!(" · 提问者 {actor}"));
    }
    let mut row = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(Caption::new(label))
        .child(Input::new(&input))
        .child(button);
    if let Some(context) = question.context {
        row = row.child(Caption::new(format!("上下文：{context}")));
    }
    row.into_any_element()
}

fn render_inspection(
    inspection: &WorkflowInspection,
    dispatch: SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let run_id = inspection.run_id.clone();
    let mut rows = Vec::<AnyElement>::new();
    if let Some(run) = &inspection.run {
        rows.push(
            Caption::new(format!(
                "运行摘要：{} · {} · 可恢复 {}{}",
                run.status,
                run.run_id,
                if run.resumable { "是" } else { "否" },
                run.stop_reason
                    .as_deref()
                    .map(|reason| format!(" · 原因 {reason}"))
                    .unwrap_or_default(),
            ))
            .into_any_element(),
        );
    }
    if let Some(barrier) = &inspection.active_barrier {
        rows.push(
            Caption::new(format!(
                "活动屏障：{} · 状态 {}",
                if barrier.active {
                    "活动"
                } else {
                    "已解除"
                },
                barrier.status
            ))
            .into_any_element(),
        );
    }
    if let Some(error_code) = &inspection.error_code {
        rows.push(Caption::new(format!("错误码：{error_code}")).into_any_element());
    }
    if let Some(frozen_run) = &inspection.frozen_run {
        rows.push(
            Caption::new(format!("冻结运行：{}", bounded_json(frozen_run))).into_any_element(),
        );
    }
    if let Some(progress_events) = &inspection.progress_events {
        rows.push(
            Caption::new(format!("进度事件：{} 条", progress_events.len())).into_any_element(),
        );
        rows.extend(progress_events.iter().map(|event| {
            Caption::new(format!(
                "进度 #{} · {} · {}",
                event.sequence,
                event.event_type,
                bounded_json(&event.payload),
            ))
            .into_any_element()
        }));

        let can_answer = workflow_inspection_is_active(inspection);
        for question in pending_workflow_questions(progress_events) {
            rows.push(render_workflow_question(
                inspection,
                question,
                can_answer,
                dispatch.clone(),
                window,
                cx,
            ));
        }
    }
    if let Some(events) = &inspection.events {
        rows.push(
            Caption::new(format!(
                "事件：{} 条{}",
                events.events.len(),
                if events.has_more {
                    "（还有更多）"
                } else {
                    ""
                }
            ))
            .into_any_element(),
        );
        rows.extend(events.events.iter().map(|event| {
            Caption::new(format!(
                "#{} · {} · {}",
                event.sequence,
                event.event_type,
                bounded_json(&event.payload),
            ))
            .into_any_element()
        }));
    }
    if let Some(graph) = &inspection.graph {
        rows.push(
            Caption::new(format!(
                "图：{} 个可见节点，{} 条边；定义 meta：{}",
                graph.nodes.len(),
                graph.edges.len(),
                bounded_json(&serde_json::to_value(&graph.definition).unwrap_or(Value::Null)),
            ))
            .into_any_element(),
        );
        rows.extend(graph.nodes.iter().map(|node| {
            Caption::new(format!(
                "节点 {} · {:?} · {} · depth {}",
                node.id, node.kind, node.label, node.depth
            ))
            .into_any_element()
        }));
    }
    if let Some(workspace) = &inspection.workspace {
        rows.push(
            Caption::new(format!(
                "工作区节点：{} 条{}",
                workspace.nodes.len(),
                if workspace.truncated {
                    "（已截断）"
                } else {
                    ""
                }
            ))
            .into_any_element(),
        );
        rows.extend(workspace.nodes.iter().map(|node| {
            let run_id = run_id.clone();
            let site_id = node.site_id.clone();
            let ordinal = node.ordinal;
            let dispatch = dispatch.clone();
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .child(Caption::new(format!(
                    "{} · {} · {}",
                    node.site_id,
                    node.status,
                    node.summary
                        .as_ref()
                        .map(bounded_json)
                        .unwrap_or_else(|| "无摘要".to_owned())
                )))
                .child(
                    Button::new(
                        format!("settings-workflow-node-result:{run_id}:{site_id}:{ordinal}"),
                        "读取节点结果",
                    )
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, window, cx| {
                        dispatch(
                            SettingsCommand::ReadWorkflowNodeResult {
                                run_id: run_id.clone(),
                                site_id: site_id.clone(),
                                ordinal,
                                max_bytes: 64 * 1024,
                            },
                            window,
                            cx,
                        )
                    }),
                )
                .into_any_element()
        }));
    }
    if let Some(result) = &inspection.node_result {
        rows.push(
            Caption::new(format!(
                "节点结果：{} · {} bytes{} · result={} · error={}",
                result.status,
                result.total_bytes,
                if result.truncated {
                    "（已截断）"
                } else {
                    ""
                },
                result
                    .result
                    .as_ref()
                    .map(bounded_json)
                    .unwrap_or_else(|| "null".to_owned()),
                result
                    .error
                    .as_ref()
                    .map(bounded_json)
                    .unwrap_or_else(|| "null".to_owned()),
            ))
            .into_any_element(),
        );
    }
    if let Some(artifacts) = &inspection.artifacts {
        rows.push(Caption::new(format!("产物元数据：{} 个", artifacts.len())).into_any_element());
        rows.extend(artifacts.iter().map(|artifact| {
            let run_id = run_id.clone();
            let artifact_id = artifact.id.clone();
            let run_id_for_bytes = run_id.clone();
            let artifact_id_for_bytes = artifact_id.clone();
            let artifact_version = artifact.version;
            let bytes_button_id = format!(
                "settings-workflow-artifact-bytes:{}:{}",
                run_id, artifact_id
            );
            let dispatch = dispatch.clone();
            let read_bytes = dispatch.clone();
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .child(Caption::new(format!(
                    "{} · {} · {} · version {} · items {} · type {}",
                    artifact.id,
                    artifact.kind,
                    artifact.title.as_deref().unwrap_or("未命名"),
                    artifact.version,
                    artifact.item_count,
                    artifact.content_type.as_deref().unwrap_or("未知"),
                )))
                .child(
                    Button::new(
                        format!("settings-workflow-artifact-items:{run_id}:{artifact_id}"),
                        "读取条目",
                    )
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |_, window, cx| {
                        dispatch(
                            SettingsCommand::ListWorkflowArtifactItems {
                                run_id: run_id.clone(),
                                artifact_id: artifact_id.clone(),
                                after_sequence: None,
                                limit: 100,
                            },
                            window,
                            cx,
                        )
                    }),
                )
                .child(
                    Button::new(bytes_button_id, "读取字节元数据")
                        .variant(ButtonVariant::Ghost)
                        .on_click(move |_, window, cx| {
                            read_bytes(
                                SettingsCommand::ReadWorkflowArtifact {
                                    run_id: run_id_for_bytes.clone(),
                                    artifact_id: artifact_id_for_bytes.clone(),
                                    version: artifact_version,
                                    offset: 0,
                                    limit: 1,
                                },
                                window,
                                cx,
                            )
                        }),
                )
                .into_any_element()
        }));
    }
    if let Some(items) = &inspection.artifact_items {
        rows.push(
            Caption::new(format!(
                "产物条目：{} 条{}",
                items.items.len(),
                if items.has_more {
                    "（还有更多）"
                } else {
                    ""
                }
            ))
            .into_any_element(),
        );
        rows.extend(
            items
                .items
                .iter()
                .map(|item| Caption::new(bounded_json(item)).into_any_element()),
        );
    }
    if let Some(bytes) = &inspection.artifact_bytes {
        rows.push(
            Caption::new(format!(
                "产物字节元数据：{} · {} bytes · next offset {:?}（不预览媒体或网页）",
                bytes.media_type, bytes.total_bytes, bytes.next_offset
            ))
            .into_any_element(),
        );
    }
    if rows.is_empty() {
        return div().into_any_element();
    }
    SettingsSection::new(format!("运行记录 · {run_id}"))
        .description("相关运行内容会按页面展示上限截断。")
        .row(SettingsRow::new("结果").control(div().flex().flex_col().gap_1().children(rows)))
        .into_any_element()
}

fn workflow_scope_value(value: &str) -> Option<&'static str> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("global") || value == "全局" {
        Some("global")
    } else if value.eq_ignore_ascii_case("project") || value == "项目" {
        Some("project")
    } else {
        None
    }
}

fn valid_scope(scope: &str) -> bool {
    workflow_scope_value(scope).is_some()
}

fn validate_new_workflow(name: &str, scope: &str, definition: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        Some("名称为空")
    } else if definition.trim().is_empty() {
        Some("定义为空")
    } else if !valid_scope(scope) {
        Some("作用域无效")
    } else {
        None
    }
}

fn optional_text(value: &str) -> Option<String> {
    let value = value.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn parse_tags(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .take(32)
        .collect()
}

fn non_empty_tags(tags: Vec<String>) -> Option<Vec<String>> {
    (!tags.is_empty()).then_some(tags)
}

fn definition_metadata(definition: &str) -> (Option<String>, Option<String>, Vec<String>) {
    let Ok(value) = serde_json::from_str::<Value>(definition) else {
        return (None, None, Vec::new());
    };
    let Some(meta) = value.get("meta").and_then(Value::as_object) else {
        return (None, None, Vec::new());
    };
    let description = meta
        .get("description")
        .and_then(Value::as_str)
        .and_then(optional_text);
    let when_to_use = meta
        .get("whenToUse")
        .or_else(|| meta.get("when_to_use"))
        .and_then(Value::as_str)
        .and_then(optional_text);
    let tags = meta
        .get("tags")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .take(32)
                .collect()
        })
        .unwrap_or_default();
    (description, when_to_use, tags)
}

fn bounded_json<T: Serialize>(value: &T) -> String {
    let text = serde_json::to_string(value).unwrap_or_else(|_| "<无法序列化>".to_owned());
    if text.chars().count() <= MAX_DISPLAY_TEXT_CHARS {
        return text;
    }
    let mut bounded = text
        .chars()
        .take(MAX_DISPLAY_TEXT_CHARS)
        .collect::<String>();
    bounded.push('…');
    bounded
}

#[cfg(test)]
mod tests {
    use super::{valid_scope, validate_new_workflow, workflow_scope_value};

    #[test]
    fn workflow_scope_keeps_protocol_values_separate_from_labels() {
        assert_eq!(workflow_scope_value("global"), Some("global"));
        assert_eq!(workflow_scope_value("全局"), Some("global"));
        assert_eq!(workflow_scope_value(" project "), Some("project"));
        assert_eq!(workflow_scope_value("项目"), Some("project"));
        assert_eq!(workflow_scope_value("workspace"), None);
        assert!(!valid_scope("workspace"));
    }

    #[test]
    fn new_workflow_validation_rejects_missing_required_values() {
        assert_eq!(validate_new_workflow("", "global", "{}"), Some("名称为空"));
        assert_eq!(
            validate_new_workflow("demo", "global", ""),
            Some("定义为空")
        );
        assert_eq!(
            validate_new_workflow("demo", "workspace", "{}"),
            Some("作用域无效")
        );
        assert_eq!(validate_new_workflow("demo", "project", "{}"), None);
    }
}
