use super::{
    SettingsCommandHandler, SettingsRow, SettingsSection, contracts::*,
    section::settings_switch_style, settings_heading,
};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{Choice, Combobox, Input, PasswordInput, Switch, TextInput},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, IconSize},
    typography::Caption,
};
use gpui::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, Role, Styled, Window, div, prelude::*,
    px,
};

use crate::native_ui::style::{UiTextSize, settings_card_color, ui_text_size};

fn model_label(model: &ModelSummary) -> String {
    if model.display_name.trim().is_empty() {
        model.id.clone()
    } else {
        format!("{} ({})", model.display_name, model.id)
    }
}

fn reasoning_effort_order(value: &str) -> usize {
    ["none", "minimal", "low", "medium", "high", "xhigh", "max"]
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(usize::MAX)
}

fn sort_reasoning_efforts(values: &mut Vec<String>) {
    values.sort_by_key(|value| (reasoning_effort_order(value), value.clone()));
    values.dedup();
}

/// Provider/model 页直接使用 provider facade 的投影。每个操作都经过 typed command
/// 返回新的 snapshot，失败时由 Panel 显示错误并保留原投影。
pub(super) fn render(
    current: &ProviderSettings,
    dispatch: SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let new_id = window.use_keyed_state("settings-provider-new-id", cx, |window, cx| {
        TextInput::new(window, cx).placeholder("provider-id")
    });
    let new_name = window.use_keyed_state("settings-provider-new-name", cx, |window, cx| {
        TextInput::new(window, cx).placeholder("供应商名称")
    });
    let new_url = window.use_keyed_state("settings-provider-new-url", cx, |window, cx| {
        TextInput::new(window, cx).placeholder("https://api.example.com/v1")
    });
    let new_key = window.use_keyed_state("settings-provider-new-key", cx, |window, cx| {
        TextInput::new(window, cx)
            .masked()
            .placeholder("API Key（可选）")
    });
    let new_model = window.use_keyed_state("settings-provider-new-model", cx, |window, cx| {
        TextInput::new(window, cx).placeholder("模型 key，例如 gpt-4o")
    });
    let new_backend = window.use_keyed_state("settings-provider-new-backend", cx, |window, cx| {
        let mut field = TextInput::new(window, cx).placeholder("chat_completions");
        field.set_text("chat_completions", cx);
        field
    });
    // Combobox 显示的是 label；创建命令只读取独立保存的协议值。
    let new_backend_value =
        window.use_keyed_state("settings-provider-new-backend-value", cx, |_, _| {
            "chat_completions".to_owned()
        });
    let create_dispatch = dispatch.clone();
    let create_id = new_id.clone();
    let create_name = new_name.clone();
    let create_url = new_url.clone();
    let create_key = new_key.clone();
    let create_model = new_model.clone();
    let create_backend_value = new_backend_value.clone();
    let create = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let id = create_id.read(cx).text().trim().to_owned();
        let name = create_name.read(cx).text().trim().to_owned();
        let base_url = create_url.read(cx).text().trim().to_owned();
        let model_id = create_model.read(cx).text().trim().to_owned();
        let api_key = (!create_key.read(cx).text().trim().is_empty())
            .then(|| create_key.read(cx).text().trim().to_owned());
        let api_backend = create_backend_value.read(cx).clone();
        // 空字段也交由真实 domain 校验，错误由 Panel 显示，避免点击后静默无反馈。
        create_dispatch(
            SettingsCommand::CreateProvider {
                id,
                name,
                base_url,
                api_backend,
                api_key,
                models: vec![model_id],
            },
            window,
            cx,
        );
    };

    let active = current.active.clone();
    let colors = cx.theme().colors.clone();
    let card_color = settings_card_color(cx.theme());
    let border_color = colors.border;
    let initial_provider_id = current
        .active
        .as_ref()
        .map(|selection| selection.provider_id.clone())
        .filter(|provider_id| {
            current
                .providers
                .iter()
                .any(|provider| &provider.id == provider_id)
        })
        .or_else(|| {
            current
                .providers
                .first()
                .map(|provider| provider.id.clone())
        });
    let selected_provider =
        window.use_keyed_state("settings-provider-selected", cx, move |_, _| {
            initial_provider_id
        });
    let selected_provider_id = selected_provider.read(cx).clone();
    let selected_provider_index = current
        .providers
        .iter()
        .position(|provider| Some(&provider.id) == selected_provider_id.as_ref())
        .unwrap_or(0);
    // 与来源 `md:grid-cols-[224px_minmax(0,1fr)]` 对齐；窄窗口保留图标栏，详情仍可操作。
    let compact_navigation = f32::from(window.viewport_size().width) < 768.0;
    let navigation_width = if compact_navigation { 56.0 } else { 224.0 };
    let detail_padding = if compact_navigation { 16.0 } else { 24.0 };
    // 对齐来源 split panel 的 p-4/pb-20 与 sm:p-6/sm:pb-24，避免详情内容贴近卡片底边。
    let detail_bottom_padding = if compact_navigation { 80.0 } else { 96.0 };
    let provider_details: Vec<(String, AnyElement)> = current
        .providers
        .iter()
        .enumerate()
        // 详情只有当前选中供应商可见，避免每帧构造其它供应商的全部模型输入实体。
        .filter(|(index, _)| *index == selected_provider_index)
        .map(|(_, provider)| {
            let provider_id = provider.id.clone();
            let provider_id_for_delete = provider.id.clone();
            let provider_id_for_refresh = provider.id.clone();
            let remove = dispatch.clone();
            let refresh = dispatch.clone();
            let title = if provider.name.trim().is_empty() {
                provider.id.clone()
            } else {
                provider.name.clone()
            };
            let edit_name = window.use_keyed_state(
                format!(
                    "settings-provider-edit-name:{}:{}",
                    provider.id, current.revision
                ),
                cx,
                |window, cx| {
                    let mut field = TextInput::new(window, cx)
                        .label(format!("编辑供应商名称：{}", provider.id))
                        .placeholder("供应商名称");
                    field.set_text(provider.name.clone(), cx);
                    field
                },
            );
            let edit_url = window.use_keyed_state(
                format!(
                    "settings-provider-edit-url:{}:{}",
                    provider.id, current.revision
                ),
                cx,
                |window, cx| {
                    let mut field = TextInput::new(window, cx)
                        .label(format!("编辑 API 地址：{}", provider.id))
                        .placeholder("API 地址");
                    field.set_text(provider.base_url.clone(), cx);
                    field
                },
            );
            let edit_backend = window.use_keyed_state(
                format!(
                    "settings-provider-edit-backend:{}:{}",
                    provider.id, current.revision
                ),
                cx,
                |window, cx| {
                    let mut field = TextInput::new(window, cx)
                        .label(format!("编辑协议：{}", provider.id))
                        .placeholder("协议");
                    field.set_text(provider.api_backend.clone(), cx);
                    field
                },
            );
            // 编辑时以 provider 快照初始化协议值，Combobox 的展示文本不会参与保存。
            let edit_backend_value = window.use_keyed_state(
                format!(
                    "settings-provider-edit-backend-value:{}:{}",
                    provider.id, current.revision
                ),
                cx,
                |_, _| provider.api_backend.clone(),
            );
            let edit_key = window.use_keyed_state(
                format!(
                    "settings-provider-edit-key:{}:{}",
                    provider.id, current.revision
                ),
                cx,
                |window, cx| {
                    TextInput::new(window, cx)
                        .masked()
                        .placeholder("留空保持原 API Key")
                },
            );
            let save_provider_dispatch = dispatch.clone();
            let save_provider_id = provider.id.clone();
            let save_provider_name = edit_name.clone();
            let save_provider_url = edit_url.clone();
            let save_provider_backend = edit_backend_value.clone();
            let save_provider_key = edit_key.clone();
            let save_provider = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                let name = save_provider_name.read(cx).text().trim().to_owned();
                let base_url = save_provider_url.read(cx).text().trim().to_owned();
                let api_backend = save_provider_backend.read(cx).clone();
                if name.is_empty() || base_url.is_empty() || !valid_api_backend(&api_backend) {
                    return;
                }
                let api_key = (!save_provider_key.read(cx).text().trim().is_empty())
                    .then(|| Some(save_provider_key.read(cx).text().trim().to_owned()));
                save_provider_dispatch(
                    SettingsCommand::UpdateProvider {
                        provider_id: save_provider_id.clone(),
                        patch: ProviderPatch {
                            name: Some(name),
                            base_url: Some(base_url),
                            api_backend: Some(api_backend),
                            api_key,
                            ..ProviderPatch::default()
                        },
                    },
                    window,
                    cx,
                );
            };
            let clear_key_dispatch = dispatch.clone();
            let clear_key_provider_id = provider.id.clone();
            let clear_api_key = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                clear_key_dispatch(
                    SettingsCommand::UpdateProvider {
                        provider_id: clear_key_provider_id.clone(),
                        patch: ProviderPatch {
                            api_key: Some(None),
                            ..ProviderPatch::default()
                        },
                    },
                    window,
                    cx,
                );
            };
            let model_switch_style = settings_switch_style(cx.theme());
            let model_rows = provider.models.iter().map(|model| {
                let model_id = model.id.clone();
                let selection = ModelSelection {
                    provider_id: provider_id.clone(),
                    model_id: model_id.clone(),
                };
                let activate = dispatch.clone();
                let patch = dispatch.clone();
                let enable_model = dispatch.clone();
                let test_model = dispatch.clone();
                let test_selection = selection.clone();
                let enabled_provider_id = provider_id.clone();
                let enabled_model_id = model_id.clone();
                // 禁用的模型仍保留设置入口；它只退出 Runtime 可选目录，便于随时重新启用。
                let enabled = Switch::new(
                    format!("settings-model-enabled:{}:{}", provider.id, model.id),
                    model.enabled,
                )
                .aria_label(format!("启用模型：{}/{}", provider.id, model.id))
                .style(model_switch_style)
                .on_change(move |enabled, window, cx| {
                    enable_model(
                        SettingsCommand::SetModelEnabled {
                            provider_id: enabled_provider_id.clone(),
                            model_id: enabled_model_id.clone(),
                            enabled,
                        },
                        window,
                        cx,
                    );
                });
                let is_active = active.as_ref().is_some_and(|active| {
                    active.provider_id == selection.provider_id
                        && active.model_id == selection.model_id
                });
                let base_config = model.config.clone();
                let model_id_for_vision = model_id.clone();
                let provider_id_for_vision = provider_id.clone();
                let model_id_for_delete = model_id.clone();
                let provider_id_for_delete = provider_id.clone();
                let vision_patch = patch.clone();
                let delete_model = patch.clone();
                let vision_config = base_config.clone();
                let vision = Switch::new(
                    format!("settings-model-vision:{}:{}", provider.id, model.id),
                    model.supports_vision,
                )
                .style(model_switch_style)
                .on_change(move |enabled, window, cx| {
                    let mut config = vision_config.clone();
                    config.supports_vision = enabled;
                    vision_patch(
                        SettingsCommand::PatchModel {
                            provider_id: provider_id_for_vision.clone(),
                            model_id: model_id_for_vision.clone(),
                            config: config.clone(),
                        },
                        window,
                        cx,
                    )
                });
                let available_efforts = current
                    .catalog
                    .iter()
                    .find(|entry| entry.provider_id == provider_id && entry.model_id == model_id)
                    .map(|entry| entry.reasoning_efforts.clone())
                    .filter(|values| !values.is_empty())
                    .unwrap_or_else(|| model.reasoning_efforts.clone());
                let selected_efforts = model.config.reasoning_efforts.clone();
                let effort_controls = available_efforts.iter().map(|effort| {
                    let effort_id = effort.clone();
                    let effort_patch = patch.clone();
                    let effort_config = base_config.clone();
                    let provider_id = provider_id.clone();
                    let model_id = model_id.clone();
                    let selected = selected_efforts.iter().any(|value| value == effort);
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(Caption::new(effort.clone()))
                        .child(
                            Switch::new(
                                format!(
                                    "settings-model-effort:{}:{}:{}",
                                    provider.id, model.id, effort
                                ),
                                selected,
                            )
                            .style(model_switch_style)
                            .on_change(move |enabled, window, cx| {
                                let mut config = effort_config.clone();
                                let mut values = config.reasoning_efforts.clone();
                                values.retain(|value| value != &effort_id);
                                if enabled {
                                    values.push(effort_id.clone());
                                }
                                sort_reasoning_efforts(&mut values);
                                config.reasoning_efforts = values;
                                config.reasoning_efforts_configured = true;
                                effort_patch(
                                    SettingsCommand::PatchModel {
                                        provider_id: provider_id.clone(),
                                        model_id: model_id.clone(),
                                        config,
                                    },
                                    window,
                                    cx,
                                )
                            }),
                        )
                });
                let restore_dispatch = patch.clone();
                let restore_config = base_config.clone();
                let restore_provider_id = provider_id.clone();
                let restore_model_id = model_id.clone();
                let reasoning_action = if model.config.reasoning_efforts_configured {
                    let restoring = selected_efforts.is_empty();
                    Button::new(
                        format!("settings-model-effort-reset:{}:{}", provider.id, model.id),
                        if restoring {
                            "恢复目录默认"
                        } else {
                            "清空推理档位"
                        },
                    )
                    .variant(ButtonVariant::Outline)
                    .on_click(move |_, window, cx| {
                        let mut config = restore_config.clone();
                        if restoring {
                            config.reasoning_efforts_configured = false;
                        } else {
                            config.reasoning_efforts.clear();
                            config.reasoning_efforts_configured = true;
                        }
                        restore_dispatch(
                            SettingsCommand::PatchModel {
                                provider_id: restore_provider_id.clone(),
                                model_id: restore_model_id.clone(),
                                config,
                            },
                            window,
                            cx,
                        )
                    })
                    .into_any_element()
                } else {
                    Caption::new(if available_efforts.is_empty() {
                        "目录未提供推理档位"
                    } else {
                        "目录默认"
                    })
                    .into_any_element()
                };
                let effort = div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(effort_controls)
                    .child(reasoning_action)
                    .into_any_element();
                let label = model_label(model);
                let model_button = Button::new(
                    format!("settings-model-activate:{}:{}", provider.id, model.id),
                    if is_active {
                        "当前模型"
                    } else {
                        "设为当前"
                    },
                )
                .variant(if is_active {
                    ButtonVariant::Subtle
                } else {
                    ButtonVariant::Outline
                })
                // 模型切换按钮的可见文案会在“当前模型/设为当前”之间变化；
                // Provider/model 组合保持稳定，原生 UIA 计划才能精确选中目标模型。
                .aria_label(format!(
                    "{}：{}/{}",
                    if is_active {
                        "当前模型"
                    } else {
                        "设为当前"
                    },
                    provider.id,
                    model.id
                ))
                .disabled(!model.enabled || !model.executable)
                .on_click(move |_, window, cx| {
                    activate(
                        SettingsCommand::SetActiveModel(selection.clone()),
                        window,
                        cx,
                    )
                });
                let delete_button = Button::new(
                    format!("settings-model-delete:{}:{}", provider.id, model.id),
                    "删除模型",
                )
                .aria_label(format!("删除模型：{}/{}", provider.id, model.id))
                .variant(ButtonVariant::Danger)
                .disabled(provider.models.len() <= 1)
                .on_click(move |_, window, cx| {
                    delete_model(
                        SettingsCommand::DeleteModel {
                            provider_id: provider_id_for_delete.clone(),
                            model_id: model_id_for_delete.clone(),
                        },
                        window,
                        cx,
                    )
                });
                let test_button = Button::new(
                    format!("settings-model-test:{}:{}", provider.id, model.id),
                    "测试模型",
                )
                .aria_label(format!("测试模型：{}/{}", provider.id, model.id))
                .variant(ButtonVariant::Outline)
                .disabled(!model.enabled || !model.executable)
                .on_click(move |_, window, cx| {
                    test_model(
                        SettingsCommand::TestModel {
                            selection: test_selection.clone(),
                        },
                        window,
                        cx,
                    );
                });
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .py_2()
                    .border_b_1()
                    .border_color(border_color)
                    .child(div().flex_1().min_w(px(180.)).max_w_full().child(label))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .max_w_full()
                            .child(Caption::new("启用"))
                            .child(enabled)
                            .child(Caption::new("视觉"))
                            .child(vision)
                            .child(Caption::new("推理"))
                            .child(div().w(px(150.)).max_w_full().child(effort))
                            .child(model_button)
                            .child(test_button)
                            .child(delete_button),
                    )
                    .into_any_element()
            });
            let add_model_dispatch = dispatch.clone();
            let add_model_field = window.use_keyed_state(
                format!("settings-provider-model-input:{provider_id}"),
                cx,
                |window, cx| TextInput::new(window, cx).placeholder("新增模型 key"),
            );
            let add_model_input = add_model_field.clone();
            let provider_id_for_add = provider_id.clone();
            let add_model = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                let model_id = add_model_input.read(cx).text().trim().to_owned();
                if model_id.is_empty() {
                    return;
                }
                add_model_dispatch(
                    SettingsCommand::CreateModel {
                        provider_id: provider_id_for_add.clone(),
                        model_id,
                        config: ModelConfig::default(),
                    },
                    window,
                    cx,
                )
            };
            let detail = div()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(180.))
                                .max_w_full()
                                .child(settings_heading(title, cx.theme())),
                        )
                        .child(
                            Button::new(
                                format!("settings-provider-refresh:{provider_id_for_refresh}"),
                                "刷新目录",
                            )
                            .variant(ButtonVariant::Outline)
                            .on_click(move |_, window, cx| {
                                refresh(
                                    SettingsCommand::RefreshProviderCatalog {
                                        provider_id: provider_id_for_refresh.clone(),
                                    },
                                    window,
                                    cx,
                                )
                            }),
                        )
                        .child(
                            Button::new(
                                format!("settings-provider-delete:{provider_id_for_delete}"),
                                "删除",
                            )
                            .variant(ButtonVariant::Danger)
                            .on_click(move |_, window, cx| {
                                remove(
                                    SettingsCommand::DeleteProvider {
                                        provider_id: provider_id_for_delete.clone(),
                                    },
                                    window,
                                    cx,
                                )
                            }),
                        ),
                )
                .child(Caption::new(format!(
                    "{} · {} · {}",
                    provider.base_url,
                    provider.api_backend,
                    if provider.api_key_configured {
                        "已配置密钥"
                    } else {
                        "未配置密钥"
                    }
                )))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(Caption::new("编辑"))
                        .child(div().w(px(190.)).max_w_full().child(Input::new(&edit_name)))
                        .child(div().w(px(300.)).max_w_full().child(Input::new(&edit_url)))
                        .child(
                            div().w(px(190.)).max_w_full().child(
                                Combobox::new(
                                    format!("settings-provider-edit-backend:{provider_id}"),
                                    &edit_backend,
                                    [
                                        Choice::new("chat_completions", "Chat Completions"),
                                        Choice::new("responses", "Responses"),
                                        Choice::new("messages", "Messages"),
                                    ],
                                )
                                .selected(edit_backend_value.read(cx).clone())
                                .on_change(
                                    move |value, _window, cx| {
                                        edit_backend_value.update(cx, |backend, cx| {
                                            *backend = value.to_string();
                                            cx.notify();
                                        });
                                    },
                                ),
                            ),
                        )
                        .child(
                            div()
                                .w(px(220.))
                                .max_w_full()
                                .child(PasswordInput::new(&edit_key)),
                        )
                        .child(
                            Button::new(
                                format!("settings-provider-save:{provider_id}"),
                                "保存供应商",
                            )
                            .variant(ButtonVariant::Outline)
                            .on_click(save_provider),
                        )
                        .child(
                            Button::new(
                                format!("settings-provider-clear-key:{provider_id}"),
                                "清除 API Key",
                            )
                            .variant(ButtonVariant::Danger)
                            .disabled(!provider.api_key_configured)
                            .on_click(clear_api_key),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(180.))
                                .max_w_full()
                                .child(Input::new(&add_model_field)),
                        )
                        .child(
                            Button::new(
                                format!("settings-provider-model-add:{provider_id}"),
                                "添加模型",
                            )
                            .variant(ButtonVariant::Outline)
                            .on_click(add_model),
                        ),
                )
                .children(model_rows)
                .into_any_element();
            (provider_id, detail)
        })
        .collect();

    let navigation_text_size = ui_text_size(cx.theme(), UiTextSize::Base);
    let navigation_rows = current
        .providers
        .iter()
        .enumerate()
        .map(|(index, provider)| {
            let provider_id = provider.id.clone();
            let label = if provider.name.trim().is_empty() {
                provider.id.clone()
            } else {
                provider.name.clone()
            };
            let selected = index == selected_provider_index;
            let click_selection = selected_provider.clone();
            let key_selection = selected_provider.clone();
            let click_provider_id = provider_id.clone();
            let key_provider_id = provider_id;
            let aria_label = label.clone();
            div()
                .id(format!("settings-provider-nav:{}", provider.id))
                .role(Role::Button)
                .aria_label(aria_label)
                .tab_index(0)
                .flex()
                .items_center()
                .h(px(32.0))
                .w_full()
                .gap_2()
                .px_2()
                .py_1()
                .rounded(px(8.0))
                .text_size(navigation_text_size)
                .font_weight(FontWeight::MEDIUM)
                .when(selected, |row| {
                    row.border_1()
                        .border_color(colors.border_strong)
                        .bg(colors.active)
                })
                .when(!selected, |row| row.hover(|style| style.bg(colors.hover)))
                .text_color(colors.fg)
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    click_selection.update(cx, |value, cx| {
                        *value = Some(click_provider_id.clone());
                        cx.notify();
                    });
                })
                .on_key_down(move |event, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        cx.stop_propagation();
                        key_selection.update(cx, |value, cx| {
                            *value = Some(key_provider_id.clone());
                            cx.notify();
                        });
                    }
                })
                .child(
                    Icon::new(IconName::Package)
                        .size(IconSize::Sm)
                        .color(colors.fg),
                )
                .when(!compact_navigation, |row| {
                    row.child(div().flex_1().min_w_0().text_color(colors.fg).child(label))
                })
        });
    let provider_navigation = div()
        .flex()
        .flex_col()
        .flex_none()
        .min_h_0()
        .w(px(navigation_width))
        .border_r_1()
        .border_color(border_color)
        .when(!compact_navigation, |view| {
            view.child(
                div()
                    .h(px(28.0))
                    .flex()
                    .items_center()
                    .px_2()
                    .text_size(ui_text_size(cx.theme(), UiTextSize::Sm))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.fg_subtle)
                    .child("供应商"),
            )
        })
        .child(
            div()
                .id("settings-provider-navigation-scroll")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap_2()
                .overflow_y_scroll()
                .px_2()
                .py_2()
                .children(navigation_rows),
        );
    let selected_detail = provider_details
        .into_iter()
        .next()
        .map(|(_, detail)| detail)
        .unwrap_or_else(|| {
            div()
                .flex()
                .items_center()
                .min_h(px(128.0))
                .text_size(ui_text_size(cx.theme(), UiTextSize::Base))
                .text_color(colors.fg_muted)
                .child("暂无可显示的模型供应商。")
                .into_any_element()
        });
    let provider_split_panel = div()
        .flex()
        .w_full()
        .min_h(px(576.0))
        .overflow_hidden()
        .rounded(px(12.0))
        .border_1()
        .border_color(border_color)
        .bg(card_color)
        .child(provider_navigation)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .p(px(detail_padding))
                .pb(px(detail_bottom_padding))
                .child(selected_detail),
        );

    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_4()
        .child(
            div()
                .text_size(ui_text_size(cx.theme(), UiTextSize::Base))
                .line_height(px(24.0))
                .text_color(colors.fg_muted)
                .child("管理自定义模型供应商，配置后可在聊天时选择使用。"),
        )
        .child(provider_split_panel)
        .child(
            SettingsSection::new("新增自定义供应商")
                .description("填写供应商信息和至少一个模型；API Key 可按接口要求填写。")
                .row(
                    SettingsRow::new("标识")
                        .control(div().w(px(250.)).max_w_full().child(Input::new(&new_id))),
                )
                .row(
                    SettingsRow::new("名称")
                        .control(div().w(px(250.)).max_w_full().child(Input::new(&new_name))),
                )
                .row(
                    SettingsRow::new("API 地址")
                        .wide()
                        .control(div().w(px(330.)).max_w_full().child(Input::new(&new_url))),
                )
                .row(
                    SettingsRow::new("协议").control(
                        div().w(px(250.)).max_w_full().child(
                            Combobox::new(
                                "settings-provider-new-backend-picker",
                                &new_backend,
                                [
                                    Choice::new("chat_completions", "Chat Completions"),
                                    Choice::new("responses", "Responses"),
                                    Choice::new("messages", "Messages"),
                                ],
                            )
                            .selected(new_backend_value.read(cx).clone())
                            .on_change(move |value, _window, cx| {
                                new_backend_value.update(cx, |backend, cx| {
                                    *backend = value.to_string();
                                    cx.notify();
                                });
                            }),
                        ),
                    ),
                )
                .row(
                    SettingsRow::new("API Key").wide().control(
                        div()
                            .w(px(330.))
                            .max_w_full()
                            .child(PasswordInput::new(&new_key)),
                    ),
                )
                .row(
                    SettingsRow::new("初始模型 key")
                        .wide()
                        .description("每个供应商至少需要一个模型 key。")
                        .control(div().w(px(330.)).max_w_full().child(Input::new(&new_model))),
                )
                .row(
                    SettingsRow::new("操作").control(
                        Button::new("settings-provider-create", "添加供应商")
                            .variant(ButtonVariant::Primary)
                            .on_click(create),
                    ),
                ),
        )
        .into_any_element()
}

fn valid_api_backend(value: &str) -> bool {
    matches!(value, "messages" | "chat_completions" | "responses")
}

#[cfg(test)]
mod tests {
    use super::valid_api_backend;

    #[test]
    fn api_backend_validation_accepts_protocol_values_only() {
        assert!(valid_api_backend("messages"));
        assert!(valid_api_backend("chat_completions"));
        assert!(valid_api_backend("responses"));
        assert!(!valid_api_backend("Chat Completions"));
        assert!(!valid_api_backend("unknown"));
    }
}
