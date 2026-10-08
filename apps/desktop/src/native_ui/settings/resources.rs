use super::{
    SettingsCommandHandler, SettingsRow, SettingsSection, contracts::*,
    section::settings_switch_style,
};
use crate::native_insights::UNSUPPORTED_PLUGIN_HOOK_REASON;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{Input, Switch, TextInput},
    theme::ActiveTheme,
};
use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, Window, div, px};
use std::collections::BTreeMap;

fn parse_key_value_lines(value: &str) -> BTreeMap<String, String> {
    value
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .filter(|(key, value)| !key.is_empty() && !value.is_empty())
        .collect()
}

fn format_key_value_lines(values: &BTreeMap<String, String>) -> String {
    values
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn set_text_input(
    window: &mut Window,
    cx: &mut App,
    key: impl Into<gpui::ElementId>,
    initial: String,
    placeholder: &'static str,
) -> gpui::Entity<TextInput> {
    window.use_keyed_state(key, cx, move |window, cx| {
        let mut field = TextInput::new(window, cx).placeholder(placeholder);
        field.set_text(initial, cx);
        field
    })
}

fn set_text_area(
    window: &mut Window,
    cx: &mut App,
    key: impl Into<gpui::ElementId>,
    initial: String,
    placeholder: &'static str,
) -> gpui::Entity<TextInput> {
    window.use_keyed_state(key, cx, move |window, cx| {
        let mut field = TextInput::new(window, cx)
            .multi_line(5, 20)
            .placeholder(placeholder);
        field.set_text(initial, cx);
        field
    })
}

fn set_labeled_text_area(
    window: &mut Window,
    cx: &mut App,
    key: impl Into<gpui::ElementId>,
    initial: String,
    label: String,
    placeholder: &'static str,
) -> gpui::Entity<TextInput> {
    window.use_keyed_state(key, cx, move |window, cx| {
        let mut field = TextInput::new(window, cx)
            .label(label.clone())
            .multi_line(5, 20)
            .placeholder(placeholder);
        field.set_text(initial, cx);
        field
    })
}

fn plugin_section(
    items: &[PluginSummary],
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let section = items.iter().fold(
        SettingsSection::new("插件").description("查看插件启用状态、公开配置和卸载操作。"),
        |section, plugin| {
            let id = plugin.id.clone();
            let id_for_remove = plugin.id.clone();
            let id_for_update = plugin.id.clone();
            let update_button_id = format!("settings-plugin-update:{id_for_update}");
            let commands_description = if plugin.commands.is_empty() {
                "当前插件没有可调用的 command。".to_owned()
            } else {
                plugin.commands.join("、")
            };
            let config_editor = set_text_area(
                window,
                cx,
                format!("settings-plugin-config:{}", plugin.id),
                format_key_value_lines(&plugin.config),
                "每行一个 key=value；仅保存插件公开配置",
            );
            let toggle = dispatch.clone();
            let remove = dispatch.clone();
            let configure = dispatch.clone();
            let update = dispatch.clone();
            let config_plugin_id = plugin.id.clone();
            let config_input = config_editor.clone();
            let section = section
                .row(
                    SettingsRow::new(plugin.name.clone())
                        .description(format!(
                            "{} · {}{}",
                            plugin.scope,
                            plugin.version.as_deref().unwrap_or("未知版本"),
                            if plugin.executable {
                                ""
                            } else {
                                " · 不可执行"
                            }
                        ))
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Switch::new(
                                        format!("settings-plugin-enabled:{id}"),
                                        plugin.enabled,
                                    )
                                    .style(settings_switch_style(cx.theme()))
                                    .on_change(
                                        move |enabled, window, cx| {
                                            toggle(
                                                SettingsCommand::SetPluginEnabled {
                                                    plugin_id: id.clone(),
                                                    enabled,
                                                },
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                )
                                .child(
                                    Button::new(update_button_id, "重新获取")
                                        .variant(ButtonVariant::Outline)
                                        .disabled(!plugin.executable || !plugin.update_available)
                                        .on_click(move |_, window, cx| {
                                            update(
                                                SettingsCommand::UpdatePlugin {
                                                    plugin_id: id_for_update.clone(),
                                                },
                                                window,
                                                cx,
                                            )
                                        }),
                                )
                                .child(
                                    Button::new(
                                        format!("settings-plugin-uninstall:{id_for_remove}"),
                                        "卸载",
                                    )
                                    .variant(ButtonVariant::Danger)
                                    .disabled(plugin.scope == "builtin")
                                    .on_click(
                                        move |_, window, cx| {
                                            remove(
                                                SettingsCommand::UninstallPlugin {
                                                    plugin_id: id_for_remove.clone(),
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
                    SettingsRow::new("公开配置")
                        .wide()
                        .description("敏感配置不会回显；不符合插件配置规则的字段会被拒绝。")
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(420.))
                                        .max_w_full()
                                        .child(Input::new(&config_editor)),
                                )
                                .child(
                                    Button::new(
                                        format!("settings-plugin-config-save:{config_plugin_id}"),
                                        "保存",
                                    )
                                    .variant(ButtonVariant::Outline)
                                    .on_click(
                                        move |_, window, cx| {
                                            configure(
                                                SettingsCommand::ConfigurePlugin {
                                                    plugin_id: config_plugin_id.clone(),
                                                    values: parse_key_value_lines(
                                                        config_input.read(cx).text(),
                                                    ),
                                                },
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                ),
                        ),
                )
                .row(SettingsRow::new("Commands").description(commands_description));
            plugin
                .unsupported_hooks
                .iter()
                .fold(section, |section, event| {
                    section.row(
                        SettingsRow::new(format!("Hook 事件：{event}")).description(format!(
                            "未支持 · 原因：{UNSUPPORTED_PLUGIN_HOOK_REASON}"
                        )),
                    )
                })
        },
    );
    section.into_any_element()
}

fn plugin_install_section(
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let source = set_text_input(
        window,
        cx,
        "settings-plugin-install-source",
        String::new(),
        "插件目录或 marketplace.json#插件名",
    );
    let install = dispatch.clone();
    SettingsSection::new("安装插件")
        .description(
            "支持本地插件目录，以及本地 marketplace.json#插件名；敏感配置仍由系统密钥库管理。",
        )
        .row(
            SettingsRow::new("插件来源")
                .wide()
                .control(div().w(px(520.)).max_w_full().child(Input::new(&source))),
        )
        .row(
            SettingsRow::new("操作").control(
                Button::new("settings-plugin-install", "安装")
                    .variant(ButtonVariant::Primary)
                    .on_click(move |_, window, cx| {
                        let source = source.read(cx).text().trim().to_owned();
                        if source.is_empty() {
                            return;
                        }
                        install(SettingsCommand::InstallPlugin { source }, window, cx);
                    }),
            ),
        )
        .into_any_element()
}

fn mcp_section(
    items: &[McpServerSummary],
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let section = items.iter().fold(
        SettingsSection::new("MCP 服务器")
            .description("显示已配置的连接状态和工具数量；查看不会启动或重试连接。"),
        |section, server| {
            let id = server.id.clone();
            let id_for_remove = server.id.clone();
            let id_for_inspect = server.id.clone();
            let config_editor = set_labeled_text_area(
                window,
                cx,
                format!("settings-mcp-config:{}", server.id),
                server.config_json.clone(),
                format!("编辑 MCP 配置：{}", server.id),
                "MCP 配置 JSON 对象",
            );
            let toggle = dispatch.clone();
            let remove = dispatch.clone();
            let inspect = dispatch.clone();
            let update = dispatch.clone();
            let update_input = config_editor.clone();
            let update_server = server.clone();
            let state = if let Some(error) = &server.error {
                format!("{} · 错误：{}", server.transport, error)
            } else if server.connected {
                format!(
                    "{} · 已连接 · {} 个工具",
                    server.transport,
                    server.tool_count.unwrap_or(0)
                )
            } else {
                format!("{} · 未连接", server.transport)
            };
            section
                .row(
                    SettingsRow::new(server.name.clone())
                        .description(state)
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Switch::new(
                                        format!("settings-mcp-enabled:{id}"),
                                        server.enabled,
                                    )
                                    .style(settings_switch_style(cx.theme()))
                                    .on_change(
                                        move |enabled, window, cx| {
                                            toggle(
                                                SettingsCommand::SetMcpEnabled {
                                                    server_id: id.clone(),
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
                                        format!("settings-mcp-inspect:{id_for_inspect}"),
                                        "读取运行态",
                                    )
                                    .variant(ButtonVariant::Outline)
                                    .on_click(
                                        move |_, window, cx| {
                                            inspect(
                                                SettingsCommand::InspectMcp {
                                                    server_id: id_for_inspect.clone(),
                                                },
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                )
                                .child(
                                    Button::new(
                                        format!("settings-mcp-remove:{id_for_remove}"),
                                        "移除",
                                    )
                                    .variant(ButtonVariant::Danger)
                                    .on_click(
                                        move |_, window, cx| {
                                            remove(
                                                SettingsCommand::RemoveMcp {
                                                    server_id: id_for_remove.clone(),
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
                    SettingsRow::new("配置 JSON")
                        .wide()
                        .description("保存前会校验传输类型、地址或命令，以及 JSON 对象结构。")
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(460.))
                                        .max_w_full()
                                        .child(Input::new(&config_editor)),
                                )
                                .child(
                                    Button::new(format!("settings-mcp-save:{}", server.id), "保存")
                                        .variant(ButtonVariant::Outline)
                                        .on_click(move |_, window, cx| {
                                            let mut next = update_server.clone();
                                            next.config_json =
                                                update_input.read(cx).text().to_owned();
                                            update(
                                                SettingsCommand::UpdateMcp { server: next },
                                                window,
                                                cx,
                                            )
                                        }),
                                ),
                        ),
                )
        },
    );
    section.into_any_element()
}

fn skill_section(
    items: &[SkillSummary],
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let section = items.iter().fold(
        SettingsSection::new("技能").description("可编辑全局或项目技能；插件提供的技能只能查看。"),
        |section, skill| {
            let id = skill.id.clone();
            let id_for_copy = skill.id.clone();
            let id_for_delete = skill.id.clone();
            // 新建表单仍使用 placeholder；已有资源编辑器带稳定 ID，避免同屏 UIA 名称冲突。
            let content_editor = set_labeled_text_area(
                window,
                cx,
                format!("settings-skill-content:{}", skill.id),
                skill.content.clone().unwrap_or_default(),
                format!("编辑 SKILL.md 内容：{}", skill.id),
                "SKILL.md 内容",
            );
            let toggle = dispatch.clone();
            let copy = dispatch.clone();
            let delete = dispatch.clone();
            let update = dispatch.clone();
            let update_input = content_editor.clone();
            let update_skill = skill.clone();
            section
                .row(
                    SettingsRow::new(skill.name.clone())
                        .description(format!(
                            "{} · {}",
                            skill.scope,
                            skill.description.as_deref().unwrap_or("无描述")
                        ))
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Switch::new(
                                        format!("settings-skill-enabled:{id}"),
                                        skill.enabled,
                                    )
                                    .style(settings_switch_style(cx.theme()))
                                    .on_change(
                                        move |enabled, window, cx| {
                                            toggle(
                                                SettingsCommand::SetSkillEnabled {
                                                    skill_id: id.clone(),
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
                                        format!("settings-skill-copy:{id_for_copy}"),
                                        "复制到通用",
                                    )
                                    .variant(ButtonVariant::Outline)
                                    .disabled(skill.scope.starts_with("plugin:"))
                                    .on_click(
                                        move |_, window, cx| {
                                            copy(
                                                SettingsCommand::CopySkillToCommon {
                                                    skill_id: id_for_copy.clone(),
                                                },
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                )
                                .child(
                                    Button::new(
                                        format!("settings-skill-delete:{id_for_delete}"),
                                        "删除",
                                    )
                                    .variant(ButtonVariant::Danger)
                                    .disabled(skill.scope.starts_with("plugin:"))
                                    .on_click(
                                        move |_, window, cx| {
                                            delete(
                                                SettingsCommand::DeleteSkill {
                                                    skill_id: id_for_delete.clone(),
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
                    SettingsRow::new("正文")
                        .wide()
                        .description(skill.path.as_deref().unwrap_or("路径不可用"))
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(520.))
                                        .max_w_full()
                                        .child(Input::new(&content_editor)),
                                )
                                .child(
                                    Button::new(
                                        format!("settings-skill-save:{}", skill.id),
                                        "保存",
                                    )
                                    .variant(ButtonVariant::Outline)
                                    .disabled(skill.scope.starts_with("plugin:"))
                                    .on_click(
                                        move |_, window, cx| {
                                            let mut next = update_skill.clone();
                                            next.content =
                                                Some(update_input.read(cx).text().to_owned());
                                            update(
                                                SettingsCommand::UpdateSkill { skill: next },
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                ),
                        ),
                )
        },
    );
    section.into_any_element()
}

fn memory_section(
    items: &[MemorySummary],
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let section = items.iter().fold(
        SettingsSection::new("记忆").description("查看和编辑全局 Markdown Memory 文件。"),
        |section, memory| {
            let workspace_id = memory.workspace_id.clone();
            let file_name = memory.file_name.clone();
            let read_workspace = workspace_id.clone();
            let read_file = file_name.clone();
            let delete_workspace = workspace_id.clone();
            let delete_file = file_name.clone();
            // 文件名是全局 Memory 的稳定标识，必须进入可访问名称以区分新增表单和编辑器。
            let content_editor = set_labeled_text_area(
                window,
                cx,
                format!(
                    "settings-memory-content:{}:{}:{}:{}",
                    workspace_id,
                    file_name,
                    memory.updated_at_ms,
                    memory.content.is_some()
                ),
                memory.content.clone().unwrap_or_default(),
                format!("编辑 Memory 内容：{}", memory.file_name),
                "Memory 内容",
            );
            let read = dispatch.clone();
            let delete = dispatch.clone();
            let update = dispatch.clone();
            let update_input = content_editor.clone();
            let update_memory = memory.clone();
            section
                .row(
                    SettingsRow::new(memory.file_name.clone())
                        .description(format!(
                            "{} · {} 字节 · {}",
                            memory.label, memory.size, memory.kind
                        ))
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Button::new(
                                        format!("settings-memory-read:{workspace_id}:{file_name}"),
                                        "查看",
                                    )
                                    .variant(ButtonVariant::Outline)
                                    .on_click(
                                        move |_, window, cx| {
                                            read(
                                                SettingsCommand::ReadMemory {
                                                    workspace_id: read_workspace.clone(),
                                                    file_name: read_file.clone(),
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
                                            "settings-memory-delete:{workspace_id}:{file_name}"
                                        ),
                                        "删除",
                                    )
                                    .variant(ButtonVariant::Danger)
                                    .on_click(
                                        move |_, window, cx| {
                                            delete(
                                                SettingsCommand::DeleteMemory {
                                                    workspace_id: delete_workspace.clone(),
                                                    file_name: delete_file.clone(),
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
                    SettingsRow::new("正文")
                        .wide()
                        .description(format!("{} · {}", memory.workspace_id, memory.file_name))
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(520.))
                                        .max_w_full()
                                        .child(Input::new(&content_editor)),
                                )
                                .child(
                                    Button::new(
                                        format!("settings-memory-save:{workspace_id}:{file_name}"),
                                        "保存",
                                    )
                                    .variant(ButtonVariant::Outline)
                                    .disabled(memory.content.is_none())
                                    .on_click(
                                        move |_, window, cx| {
                                            let next = MemoryFile {
                                                workspace_id: update_memory.workspace_id.clone(),
                                                file_name: update_memory.file_name.clone(),
                                                content: update_input.read(cx).text().to_owned(),
                                                updated_at_ms: update_memory.updated_at_ms,
                                            };
                                            update(
                                                SettingsCommand::UpdateMemory { memory: next },
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                ),
                        ),
                )
        },
    );
    section.into_any_element()
}

fn template_section(
    items: &[AgentTemplateSummary],
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let section = items.iter().fold(
        SettingsSection::new("Agent 模板")
            .description("模板用于创建智能体时提供默认配置；未知字段不会被当作已生效能力。"),
        |section, template| {
            let id = template.id.clone();
            let id_for_delete = template.id.clone();
            let id_for_save = template.id.clone();
            // 模板 ID 区分同名资源的作用域，避免多个模板编辑器共享同一 UIA 名称。
            let content_editor = set_labeled_text_area(
                window,
                cx,
                format!("settings-template-content:{}", template.id),
                template.content.clone().unwrap_or_default(),
                format!("编辑 Agent 模板正文：{}", template.id),
                "Agent 模板正文",
            );
            let toggle = dispatch.clone();
            let delete = dispatch.clone();
            let update = dispatch.clone();
            let update_input = content_editor.clone();
            let update_template = template.clone();
            section
                .row(
                    SettingsRow::new(template.name.clone())
                        .description(format!(
                            "{} · {} 工具 · {}",
                            template.scope,
                            template.tools.len(),
                            template.description.as_deref().unwrap_or("无描述")
                        ))
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Switch::new(
                                        format!("settings-template-enabled:{id}"),
                                        template.enabled,
                                    )
                                    .style(settings_switch_style(cx.theme()))
                                    .on_change(
                                        move |enabled, window, cx| {
                                            toggle(
                                                SettingsCommand::SetAgentTemplateEnabled {
                                                    agent_id: id.clone(),
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
                                        format!("settings-template-delete:{id_for_delete}"),
                                        "删除",
                                    )
                                    .variant(ButtonVariant::Danger)
                                    .on_click(
                                        move |_, window, cx| {
                                            delete(
                                                SettingsCommand::DeleteAgentTemplate {
                                                    agent_id: id_for_delete.clone(),
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
                    SettingsRow::new("正文")
                        .wide()
                        .description("保存时保留当前模板元数据，并重新写入模板文件。")
                        .control(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(520.))
                                        .max_w_full()
                                        .child(Input::new(&content_editor)),
                                )
                                .child(
                                    Button::new(
                                        format!("settings-template-save:{id_for_save}"),
                                        "保存",
                                    )
                                    .variant(ButtonVariant::Outline)
                                    .on_click(
                                        move |_, window, cx| {
                                            let mut next = update_template.clone();
                                            next.content =
                                                Some(update_input.read(cx).text().to_owned());
                                            update(
                                                SettingsCommand::UpdateAgentTemplate(next),
                                                window,
                                                cx,
                                            )
                                        },
                                    ),
                                ),
                        ),
                )
        },
    );
    section.into_any_element()
}

/// 资源设置使用一个 SettingsPage::Resources 快照；此枚举只负责页面内的展示路由。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum ResourceCategory {
    #[default]
    Plugins,
    Mcp,
    Skills,
    Memory,
    AgentTemplates,
}

impl ResourceCategory {
    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::Plugins => "插件",
            Self::Mcp => "MCP",
            Self::Skills => "技能",
            Self::Memory => "记忆",
            Self::AgentTemplates => "Agent 模板",
        }
    }

    pub(super) const fn key(self) -> &'static str {
        match self {
            Self::Plugins => "plugins",
            Self::Mcp => "mcp",
            Self::Skills => "skills",
            Self::Memory => "memory",
            Self::AgentTemplates => "agent-templates",
        }
    }
}

fn render_plugins(
    current: &ResourceSettings,
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let plugin_install = plugin_install_section(dispatch, window, cx);
    let plugins = plugin_section(&current.plugins, dispatch, window, cx);
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_5()
        .child(plugin_install)
        .child(plugins)
        .into_any_element()
}

fn mcp_create_section(
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let mcp_name = set_text_input(
        window,
        cx,
        "settings-mcp-new-name",
        String::new(),
        "服务器名称",
    );
    let mcp_scope = set_text_input(
        window,
        cx,
        "settings-mcp-new-scope",
        "global".to_owned(),
        "global 或 project",
    );
    let mcp_transport = set_text_input(
        window,
        cx,
        "settings-mcp-new-transport",
        "stdio".to_owned(),
        "stdio 或 http",
    );
    let mcp_config = set_text_area(
        window,
        cx,
        "settings-mcp-new-config",
        "{}".to_owned(),
        "MCP 配置 JSON 对象",
    );
    let create_mcp_name = mcp_name.clone();
    let create_mcp_scope = mcp_scope.clone();
    let create_mcp_transport = mcp_transport.clone();
    let create_mcp_config = mcp_config.clone();
    let create_mcp_dispatch = dispatch.clone();
    let create_mcp = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let name = create_mcp_name.read(cx).text().trim().to_owned();
        let scope = create_mcp_scope.read(cx).text().trim().to_owned();
        let transport = create_mcp_transport.read(cx).text().trim().to_owned();
        let config_json = create_mcp_config.read(cx).text().trim().to_owned();
        if name.is_empty() || scope.is_empty() || transport.is_empty() || config_json.is_empty() {
            return;
        }
        let id = if scope == "global" {
            name.clone()
        } else {
            format!("{scope}:{name}")
        };
        create_mcp_dispatch(
            SettingsCommand::AddMcp {
                server: McpServerSummary {
                    id,
                    scope,
                    name,
                    transport,
                    enabled: true,
                    connected: false,
                    tool_count: None,
                    error: None,
                    config_json,
                },
            },
            window,
            cx,
        );
    };

    SettingsSection::new("新增 MCP 服务器")
        .description("作用域、传输类型和配置对象会在保存时校验。")
        .row(
            SettingsRow::new("名称")
                .control(div().w(px(300.)).max_w_full().child(Input::new(&mcp_name))),
        )
        .row(
            SettingsRow::new("作用域")
                .control(div().w(px(220.)).max_w_full().child(Input::new(&mcp_scope))),
        )
        .row(
            SettingsRow::new("传输").control(
                div()
                    .w(px(220.))
                    .max_w_full()
                    .child(Input::new(&mcp_transport)),
            ),
        )
        .row(
            SettingsRow::new("配置 JSON").wide().control(
                div()
                    .w(px(520.))
                    .max_w_full()
                    .child(Input::new(&mcp_config)),
            ),
        )
        .row(
            SettingsRow::new("操作").control(
                Button::new("settings-mcp-create", "添加服务器")
                    .variant(ButtonVariant::Primary)
                    .on_click(create_mcp),
            ),
        )
        .into_any_element()
}

fn render_mcp(
    current: &ResourceSettings,
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let create = mcp_create_section(dispatch, window, cx);
    let servers = mcp_section(&current.mcp_servers, dispatch, window, cx);
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_5()
        .child(create)
        .child(servers)
        .into_any_element()
}

fn skill_create_section(
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let skill_name = set_text_input(
        window,
        cx,
        "settings-skill-new-name",
        String::new(),
        "Skill 目录名",
    );
    let skill_scope = set_text_input(
        window,
        cx,
        "settings-skill-new-scope",
        "global".to_owned(),
        "global 或 project",
    );
    let skill_content = set_text_area(
        window,
        cx,
        "settings-skill-new-content",
        String::new(),
        "SKILL.md 内容",
    );
    let create_skill_name = skill_name.clone();
    let create_skill_scope = skill_scope.clone();
    let create_skill_content = skill_content.clone();
    let create_skill_dispatch = dispatch.clone();
    let create_skill = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let name = create_skill_name.read(cx).text().trim().to_owned();
        let scope = create_skill_scope.read(cx).text().trim().to_owned();
        let content = create_skill_content.read(cx).text().to_owned();
        if name.is_empty() || scope.is_empty() || content.trim().is_empty() {
            return;
        }
        create_skill_dispatch(
            SettingsCommand::CreateSkill {
                skill: SkillSummary {
                    id: format!("{scope}:{name}"),
                    name,
                    scope,
                    enabled: true,
                    description: None,
                    path: None,
                    content: Some(content),
                },
            },
            window,
            cx,
        );
    };

    SettingsSection::new("新增技能")
        .description("保存后写入全局或当前项目的技能文件。")
        .row(
            SettingsRow::new("名称").control(
                div()
                    .w(px(300.))
                    .max_w_full()
                    .child(Input::new(&skill_name)),
            ),
        )
        .row(
            SettingsRow::new("作用域").control(
                div()
                    .w(px(220.))
                    .max_w_full()
                    .child(Input::new(&skill_scope)),
            ),
        )
        .row(
            SettingsRow::new("正文").wide().control(
                div()
                    .w(px(520.))
                    .max_w_full()
                    .child(Input::new(&skill_content)),
            ),
        )
        .row(
            SettingsRow::new("操作").control(
                Button::new("settings-skill-create", "创建技能")
                    .variant(ButtonVariant::Primary)
                    .on_click(create_skill),
            ),
        )
        .into_any_element()
}

fn render_skills(
    current: &ResourceSettings,
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let create = skill_create_section(dispatch, window, cx);
    let skills = skill_section(&current.skills, dispatch, window, cx);
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_5()
        .child(create)
        .child(skills)
        .into_any_element()
}

fn memory_create_section(
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let memory_name = set_text_input(
        window,
        cx,
        "settings-memory-new-name",
        String::new(),
        "文件名，例如 project.md",
    );
    let memory_content = set_text_area(
        window,
        cx,
        "settings-memory-new-content",
        String::new(),
        "Memory 内容",
    );
    let create_memory_name = memory_name.clone();
    let create_memory_content = memory_content.clone();
    let create_memory_dispatch = dispatch.clone();
    let create_memory = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let file_name = create_memory_name.read(cx).text().trim().to_owned();
        let content = create_memory_content.read(cx).text().to_owned();
        if file_name.is_empty() || content.trim().is_empty() {
            return;
        }
        create_memory_dispatch(
            SettingsCommand::CreateMemory {
                memory: MemoryFile {
                    workspace_id: "global".to_owned(),
                    file_name,
                    content,
                    updated_at_ms: 0,
                },
            },
            window,
            cx,
        );
    };

    SettingsSection::new("新增记忆")
        .description("当前设置域允许编辑全局 Markdown Memory。")
        .row(
            SettingsRow::new("文件名").control(
                div()
                    .w(px(300.))
                    .max_w_full()
                    .child(Input::new(&memory_name)),
            ),
        )
        .row(
            SettingsRow::new("正文").wide().control(
                div()
                    .w(px(520.))
                    .max_w_full()
                    .child(Input::new(&memory_content)),
            ),
        )
        .row(
            SettingsRow::new("操作").control(
                Button::new("settings-memory-create", "创建记忆")
                    .variant(ButtonVariant::Primary)
                    .on_click(create_memory),
            ),
        )
        .into_any_element()
}

fn render_memory(
    current: &ResourceSettings,
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let create = memory_create_section(dispatch, window, cx);
    let memories = memory_section(&current.memories, dispatch, window, cx);
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_5()
        .child(create)
        .child(memories)
        .into_any_element()
}

fn template_create_section(
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let template_name = set_text_input(
        window,
        cx,
        "settings-template-new-name",
        String::new(),
        "模板文件名",
    );
    let template_scope = set_text_input(
        window,
        cx,
        "settings-template-new-scope",
        "global".to_owned(),
        "global 或 project",
    );
    let template_content = set_text_area(
        window,
        cx,
        "settings-template-new-content",
        String::new(),
        "Agent 模板正文",
    );
    let create_template_name = template_name.clone();
    let create_template_scope = template_scope.clone();
    let create_template_content = template_content.clone();
    let create_template_dispatch = dispatch.clone();
    let create_template = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let name = create_template_name.read(cx).text().trim().to_owned();
        let scope = create_template_scope.read(cx).text().trim().to_owned();
        let content = create_template_content.read(cx).text().to_owned();
        if name.is_empty() || scope.is_empty() || content.trim().is_empty() {
            return;
        }
        create_template_dispatch(
            SettingsCommand::CreateAgentTemplate(AgentTemplateSummary {
                id: format!("{scope}:{name}"),
                name,
                scope,
                enabled: true,
                model: None,
                tools: Vec::new(),
                max_turns: None,
                description: None,
                content: Some(content),
            }),
            window,
            cx,
        );
    };

    SettingsSection::new("新增 Agent 模板")
        .description("保存后写入全局或当前项目的模板文件。")
        .row(
            SettingsRow::new("名称").control(
                div()
                    .w(px(300.))
                    .max_w_full()
                    .child(Input::new(&template_name)),
            ),
        )
        .row(
            SettingsRow::new("作用域").control(
                div()
                    .w(px(220.))
                    .max_w_full()
                    .child(Input::new(&template_scope)),
            ),
        )
        .row(
            SettingsRow::new("正文").wide().control(
                div()
                    .w(px(520.))
                    .max_w_full()
                    .child(Input::new(&template_content)),
            ),
        )
        .row(
            SettingsRow::new("操作").control(
                Button::new("settings-template-create", "创建模板")
                    .variant(ButtonVariant::Primary)
                    .on_click(create_template),
            ),
        )
        .into_any_element()
}

fn render_agent_templates(
    current: &ResourceSettings,
    dispatch: &SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let create = template_create_section(dispatch, window, cx);
    let templates = template_section(&current.agent_templates, dispatch, window, cx);
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_5()
        .child(create)
        .child(templates)
        .into_any_element()
}

pub(super) fn render(
    current: &ResourceSettings,
    category: ResourceCategory,
    dispatch: SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    // 分类切换只改变投影路由；分支内才创建对应的输入状态，避免每帧初始化五类草稿。
    match category {
        ResourceCategory::Plugins => render_plugins(current, &dispatch, window, cx),
        ResourceCategory::Mcp => render_mcp(current, &dispatch, window, cx),
        ResourceCategory::Skills => render_skills(current, &dispatch, window, cx),
        ResourceCategory::Memory => render_memory(current, &dispatch, window, cx),
        ResourceCategory::AgentTemplates => render_agent_templates(current, &dispatch, window, cx),
    }
}
