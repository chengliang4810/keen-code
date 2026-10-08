use super::{
    SettingsCommandHandler, SettingsRow, SettingsSection,
    contracts::*,
    section::{settings_input_style, settings_select_indicator, settings_switch_style},
};
use crate::app_updates::{AppUpdateDownloadState, AppUpdateStatus};
use crate::personalization::MAX_CUSTOM_INSTRUCTIONS_CHARS;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{Choice, Combobox, Input, Switch, TextInput},
    theme::{
        ActiveTheme, CodeRenderSettings, CodeSyntaxTheme as RuntimeCodeSyntaxTheme, ControlSize,
        Mode, Theme,
    },
    typography::Caption,
};
use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, Window, div, px};

use crate::app_settings::{
    AppUpdateDownloadSource, MAX_BACKGROUND_AGENT_LIMIT, PreferencesPatch, TerminalShell,
};

fn density_value(value: UiDensity) -> ely_gpui_component::theme::Density {
    match value {
        UiDensity::Compact => ely_gpui_component::theme::Density::Compact,
        UiDensity::Standard => ely_gpui_component::theme::Density::Standard,
        UiDensity::Comfortable => ely_gpui_component::theme::Density::Comfortable,
    }
}

fn resolve_font_family(value: &str) -> String {
    match value.trim() {
        "" | "系统默认" | ".SystemUIFont" => {
            crate::native_ui::style::default_ui_font_family().to_owned()
        }
        value => value.to_owned(),
    }
}

fn code_syntax_theme(value: CodeSyntaxTheme) -> RuntimeCodeSyntaxTheme {
    match value {
        CodeSyntaxTheme::GitHubLight => RuntimeCodeSyntaxTheme::GitHubLight,
        CodeSyntaxTheme::GitHubDark => RuntimeCodeSyntaxTheme::GitHubDark,
        CodeSyntaxTheme::Ely => RuntimeCodeSyntaxTheme::Ely,
        CodeSyntaxTheme::Quiet => RuntimeCodeSyntaxTheme::Quiet,
        CodeSyntaxTheme::Paper => RuntimeCodeSyntaxTheme::Paper,
    }
}

fn code_render_settings(value: &CodeAppearanceSettings) -> CodeRenderSettings {
    CodeRenderSettings {
        font_family: value.font_family.clone().into(),
        font_size_px: value.font_size_px,
        show_line_numbers: value.show_line_numbers,
        wrap_long_lines: value.wrap_long_lines,
        light_theme: code_syntax_theme(value.light_theme),
        dark_theme: code_syntax_theme(value.dark_theme),
    }
}

fn terminal_shell_value(value: TerminalShell) -> &'static str {
    match value {
        TerminalShell::Auto => "auto",
        TerminalShell::PowerShell => "powerShell",
        TerminalShell::PowerShell7 => "powerShell7",
        TerminalShell::GitBash => "gitBash",
        TerminalShell::Cmd => "cmd",
    }
}

fn terminal_shell_from_value(value: &str) -> Option<TerminalShell> {
    match value {
        "auto" => Some(TerminalShell::Auto),
        "powerShell" => Some(TerminalShell::PowerShell),
        "powerShell7" => Some(TerminalShell::PowerShell7),
        "gitBash" => Some(TerminalShell::GitBash),
        "cmd" => Some(TerminalShell::Cmd),
        _ => None,
    }
}

fn update_source_value(value: AppUpdateDownloadSource) -> &'static str {
    match value {
        AppUpdateDownloadSource::Auto => "auto",
        AppUpdateDownloadSource::Github => "github",
        AppUpdateDownloadSource::ChinaMirror => "chinaMirror",
    }
}

fn update_source_from_value(value: &str) -> Option<AppUpdateDownloadSource> {
    match value {
        "auto" => Some(AppUpdateDownloadSource::Auto),
        "github" => Some(AppUpdateDownloadSource::Github),
        "chinaMirror" => Some(AppUpdateDownloadSource::ChinaMirror),
        _ => None,
    }
}

fn preferences_patch(preferences: PreferencesPatch) -> GeneralPatch {
    GeneralPatch {
        preferences: Some(preferences),
        ..GeneralPatch::default()
    }
}

fn update_is_busy(status: &AppUpdateStatus) -> bool {
    matches!(
        status.download_state,
        AppUpdateDownloadState::Downloading
            | AppUpdateDownloadState::Verifying
            | AppUpdateDownloadState::Installing
    )
}

fn update_summary(status: Option<&AppUpdateStatus>) -> String {
    let Some(status) = status else {
        return "更新服务尚未就绪".to_owned();
    };
    if let Some(error) = status
        .download_error
        .as_deref()
        .filter(|error| !error.trim().is_empty())
    {
        return format!("更新失败：{error}");
    }
    match status.download_state {
        AppUpdateDownloadState::Downloading => match status.total_bytes {
            Some(total) => format!("正在下载更新 {}/{} 字节", status.downloaded_bytes, total),
            None => format!("正在下载更新 {} 字节", status.downloaded_bytes),
        },
        AppUpdateDownloadState::Verifying => "正在校验更新签名".to_owned(),
        AppUpdateDownloadState::Ready => "更新已下载并通过签名校验".to_owned(),
        AppUpdateDownloadState::Installing => "正在安装更新并准备重启".to_owned(),
        AppUpdateDownloadState::Failed => "更新下载失败，请重试".to_owned(),
        AppUpdateDownloadState::Idle if status.available => status
            .latest_release
            .as_deref()
            .or(status.latest_version.as_deref())
            .map(|version| format!("发现可用版本 {version}"))
            .unwrap_or_else(|| "发现可用更新".to_owned()),
        AppUpdateDownloadState::Idle if status.checked => "检查完成，暂无可用更新".to_owned(),
        AppUpdateDownloadState::Idle => "尚未检查更新".to_owned(),
    }
}

fn about_section(status: Option<&AppUpdateStatus>, dispatch: SettingsCommandHandler) -> AnyElement {
    let current_version = status
        .map(|value| value.current_release.clone())
        .unwrap_or_else(|| "不可用".to_owned());
    let latest_version = status
        .and_then(|value| {
            value
                .latest_release
                .clone()
                .or_else(|| value.latest_version.clone())
        })
        .unwrap_or_else(|| "未检查".to_owned());
    let summary = update_summary(status);
    let notes = status
        .and_then(|value| value.notes.clone())
        .unwrap_or_else(|| "暂无发布说明".to_owned());
    let can_check = status.is_some_and(|value| !update_is_busy(value));
    let can_download = status.is_some_and(|value| {
        value.available
            && matches!(
                value.download_state,
                AppUpdateDownloadState::Idle | AppUpdateDownloadState::Failed
            )
    });
    let can_install =
        status.is_some_and(|value| value.download_state == AppUpdateDownloadState::Ready);

    let check_dispatch = dispatch.clone();
    let check = Button::new("settings-update-check", "检查更新")
        .variant(ButtonVariant::Outline)
        .disabled(!can_check)
        .on_click(move |_, window, cx| {
            check_dispatch(SettingsCommand::CheckForUpdates, window, cx)
        });
    let download_dispatch = dispatch.clone();
    let download = Button::new("settings-update-download", "下载更新")
        .variant(ButtonVariant::Outline)
        .disabled(!can_download)
        .on_click(move |_, window, cx| {
            download_dispatch(SettingsCommand::DownloadUpdate, window, cx)
        });
    let install_dispatch = dispatch;
    let install = Button::new("settings-update-install", "安装并重启")
        .variant(ButtonVariant::Primary)
        .disabled(!can_install)
        .on_click(move |_, window, cx| {
            install_dispatch(SettingsCommand::InstallUpdate, window, cx)
        });

    SettingsSection::new("关于 KeenCode")
        .without_header()
        .row(SettingsRow::new("当前版本").control(div().child(Caption::new(current_version))))
        .row(
            SettingsRow::new("更新状态")
                .description(summary)
                .control(div().child(Caption::new(format!("目标版本：{latest_version}")))),
        )
        .row(
            SettingsRow::new("发布说明")
                .wide()
                .control(div().w(px(520.)).child(Caption::new(notes))),
        )
        .row(
            SettingsRow::new("更新操作").control(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(check)
                    .child(download)
                    .child(install),
            ),
        )
        .into_any_element()
}

/// 将已确认的常规设置立即应用到当前 GPUI App。
///
/// 持久化仍由 NativeSettingsService 完成；该函数只修改 GPUI 的全局 Theme，确保
/// 用户调整外观后当前窗口立即重绘，启动阶段也可以复用同一入口。
pub fn apply_general_settings(settings: &GeneralSettings, cx: &mut App) {
    let mode = match settings.appearance {
        AppearanceMode::System => Mode::from(cx.window_appearance()),
        AppearanceMode::Light => Mode::Light,
        AppearanceMode::Dark => Mode::Dark,
    };
    crate::native_ui::style::install_palettes(cx);
    Theme::set_mode_now(mode, cx);
    Theme::update(cx, |theme| {
        theme.density = density_value(settings.density);
        theme.font_scale = settings.font_size_px / 14.0;
        theme.font_family = resolve_font_family(&settings.font_family).into();
        theme.mono_family = crate::native_ui::style::DEFAULT_MONO_FONT_FAMILY.into();
        theme.reduced_motion = settings.reduced_motion;
    });
    Theme::set_code_render_settings(code_render_settings(&settings.code), cx);
}

/// 常规页沿用设置页本地的分组和行令牌，主题、密度和字号仍由 Ely Theme 提供。
pub(super) fn render(
    current: &GeneralSettings,
    dispatch: SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let terminal_shell_input =
        window.use_keyed_state("settings-general-terminal-shell", cx, |window, cx| {
            TextInput::new(window, cx).label("集成终端Shell")
        });
    let terminal_shell_dispatch = dispatch.clone();
    let terminal_shell = Combobox::new(
        "settings-general-terminal-shell",
        &terminal_shell_input,
        [
            Choice::new("auto", "自动选择"),
            Choice::new("powerShell", "PowerShell"),
            Choice::new("powerShell7", "PowerShell 7"),
            Choice::new("gitBash", "Git Bash"),
            Choice::new("cmd", "CMD"),
        ],
    )
    .selected(terminal_shell_value(current.terminal_shell))
    .size(ControlSize::Lg)
    .input_style(settings_input_style(cx.theme(), true, true))
    .indicator(settings_select_indicator(cx.theme()))
    .on_change(move |value, window, cx| {
        let Some(next) = terminal_shell_from_value(value.as_ref()) else {
            return;
        };
        terminal_shell_dispatch(
            SettingsCommand::SaveGeneral(GeneralPatch {
                terminal_shell: Some(next),
                ..GeneralPatch::default()
            }),
            window,
            cx,
        );
    });

    let terminal_profile_dispatch = dispatch.clone();
    let terminal_profile = Switch::new(
        "settings-general-terminal-inherit-profile",
        current.terminal_inherit_system_profile,
    )
    .style(settings_switch_style(cx.theme()))
    .on_change(move |next, window, cx| {
        terminal_profile_dispatch(
            SettingsCommand::SaveGeneral(GeneralPatch {
                terminal_inherit_system_profile: Some(next),
                ..GeneralPatch::default()
            }),
            window,
            cx,
        )
    });

    let terminal_font =
        window.use_keyed_state("settings-general-terminal-font", cx, |window, cx| {
            let mut field = TextInput::new(window, cx)
                .label("终端字体")
                .placeholder("留空恢复平台默认字体；填写后用于新建 KeenCode 终端。");
            field.set_text(current.terminal_font_family.clone(), cx);
            field
        });
    let terminal_font_input = terminal_font.clone();
    let terminal_font_dispatch = dispatch.clone();
    let terminal_font_change = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let value = terminal_font_input.read(cx).text().trim().to_owned();
        terminal_font_dispatch(
            SettingsCommand::SaveGeneral(GeneralPatch {
                terminal_font_family: Some(value),
                ..GeneralPatch::default()
            }),
            window,
            cx,
        );
    };

    let update_source_input =
        window.use_keyed_state("settings-general-update-source", cx, |window, cx| {
            TextInput::new(window, cx).label("更新下载源")
        });
    let update_source_dispatch = dispatch.clone();
    let update_source = Combobox::new(
        "settings-general-update-source",
        &update_source_input,
        [
            Choice::new("auto", "自动选择"),
            Choice::new("github", "GitHub"),
            Choice::new("chinaMirror", "国内镜像"),
        ],
    )
    .selected(update_source_value(
        current.preferences.app_update_download_source,
    ))
    .size(ControlSize::Lg)
    .input_style(settings_input_style(cx.theme(), true, true))
    .indicator(settings_select_indicator(cx.theme()))
    .on_change(move |value, window, cx| {
        let Some(next) = update_source_from_value(value.as_ref()) else {
            return;
        };
        update_source_dispatch(
            SettingsCommand::SaveGeneral(preferences_patch(PreferencesPatch {
                app_update_download_source: Some(next),
                ..PreferencesPatch::default()
            })),
            window,
            cx,
        );
    });

    let task_notifications_dispatch = dispatch.clone();
    let task_notifications = Switch::new(
        "settings-general-task-notifications",
        current.preferences.task_notifications,
    )
    .style(settings_switch_style(cx.theme()))
    .aria_label("任务通知")
    .on_change(move |next, window, cx| {
        task_notifications_dispatch(
            SettingsCommand::SaveGeneral(preferences_patch(PreferencesPatch {
                task_notifications: Some(next),
                ..PreferencesPatch::default()
            })),
            window,
            cx,
        )
    });

    let notification_sound_dispatch = dispatch.clone();
    let notification_sound = Switch::new(
        "settings-general-notification-sound",
        current.preferences.notification_sound,
    )
    .style(settings_switch_style(cx.theme()))
    .aria_label("通知提示音")
    .on_change(move |next, window, cx| {
        notification_sound_dispatch(
            SettingsCommand::SaveGeneral(preferences_patch(PreferencesPatch {
                notification_sound: Some(next),
                ..PreferencesPatch::default()
            })),
            window,
            cx,
        )
    });

    let close_to_tray_dispatch = dispatch.clone();
    let close_to_tray = Switch::new(
        "settings-general-close-to-tray",
        current.preferences.close_to_tray,
    )
    .style(settings_switch_style(cx.theme()))
    .aria_label("关闭时驻留托盘")
    .on_change(move |next, window, cx| {
        close_to_tray_dispatch(
            SettingsCommand::SaveGeneral(preferences_patch(PreferencesPatch {
                close_to_tray: Some(next),
                ..PreferencesPatch::default()
            })),
            window,
            cx,
        )
    });

    let keep_awake_dispatch = dispatch.clone();
    let keep_awake = Switch::new(
        "settings-general-keep-computer-awake",
        current.preferences.keep_computer_awake,
    )
    .style(settings_switch_style(cx.theme()))
    .aria_label("保持电脑唤醒")
    .on_change(move |next, window, cx| {
        keep_awake_dispatch(
            SettingsCommand::SaveGeneral(preferences_patch(PreferencesPatch {
                keep_computer_awake: Some(next),
                ..PreferencesPatch::default()
            })),
            window,
            cx,
        )
    });

    let local_memories_dispatch = dispatch.clone();
    let local_memories = Switch::new(
        "settings-general-local-memories",
        current.preferences.local_memories,
    )
    .style(settings_switch_style(cx.theme()))
    .aria_label("本地记忆")
    .on_change(move |next, window, cx| {
        local_memories_dispatch(
            SettingsCommand::SaveGeneral(preferences_patch(PreferencesPatch {
                local_memories: Some(next),
                ..PreferencesPatch::default()
            })),
            window,
            cx,
        )
    });

    let initial_project_directory = current.preferences.project_directory.clone();
    let project_directory = window.use_keyed_state(
        "settings-general-project-directory",
        cx,
        move |window, cx| {
            let mut field = TextInput::new(window, cx)
                .label("默认项目目录")
                .placeholder("留空使用默认项目目录");
            field.set_text(initial_project_directory.clone(), cx);
            field
        },
    );
    let project_directory_input = project_directory.clone();
    let project_directory_dispatch = dispatch.clone();
    let save_project_directory = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        project_directory_dispatch(
            SettingsCommand::SaveGeneral(preferences_patch(PreferencesPatch {
                project_directory: Some(project_directory_input.read(cx).text().trim().to_owned()),
                ..PreferencesPatch::default()
            })),
            window,
            cx,
        );
    };

    let initial_background_agent_limit = current.preferences.background_agent_limit;
    let background_agent_limit = window.use_keyed_state(
        "settings-general-background-agent-limit",
        cx,
        move |window, cx| {
            let mut field = TextInput::new(window, cx)
                .label("后台 Agent 并发上限")
                .placeholder("1-999");
            field.set_text(initial_background_agent_limit.to_string(), cx);
            field
        },
    );
    let background_agent_limit_input = background_agent_limit.clone();
    let background_agent_dispatch = dispatch.clone();
    let save_background_agent_limit =
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
            let Ok(value) = background_agent_limit_input
                .read(cx)
                .text()
                .trim()
                .parse::<u16>()
            else {
                return;
            };
            if !(1..=MAX_BACKGROUND_AGENT_LIMIT).contains(&value) {
                return;
            }
            background_agent_dispatch(
                SettingsCommand::SaveGeneral(preferences_patch(PreferencesPatch {
                    background_agent_limit: Some(value),
                    ..PreferencesPatch::default()
                })),
                window,
                cx,
            );
        };

    let initial_http_proxy = current.preferences.http_proxy.clone().unwrap_or_default();
    let http_proxy =
        window.use_keyed_state("settings-general-http-proxy", cx, move |window, cx| {
            let mut field = TextInput::new(window, cx)
                .label("HTTP 代理")
                .placeholder("例如 http://127.0.0.1:7890");
            field.set_text(initial_http_proxy.clone(), cx);
            field
        });
    let initial_no_proxy = current
        .preferences
        .http_proxy_no_proxy
        .clone()
        .unwrap_or_default();
    let http_proxy_no_proxy = window.use_keyed_state(
        "settings-general-http-proxy-no-proxy",
        cx,
        move |window, cx| {
            let mut field = TextInput::new(window, cx)
                .label("HTTP 代理绕过规则")
                .placeholder("例如 localhost,127.0.0.1");
            field.set_text(initial_no_proxy.clone(), cx);
            field
        },
    );
    let http_proxy_input = http_proxy.clone();
    let http_proxy_no_proxy_input = http_proxy_no_proxy.clone();
    let proxy_dispatch = dispatch.clone();
    // 代理和绕过规则属于同一出口策略，沿用既有一次保存同时提交两个字段的语义。
    let save_proxy = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let proxy = http_proxy_input.read(cx).text().trim().to_owned();
        let no_proxy = http_proxy_no_proxy_input.read(cx).text().trim().to_owned();
        proxy_dispatch(
            SettingsCommand::SaveGeneral(preferences_patch(PreferencesPatch {
                http_proxy: Some((!proxy.is_empty()).then_some(proxy)),
                http_proxy_no_proxy: Some((!no_proxy.is_empty()).then_some(no_proxy)),
                ..PreferencesPatch::default()
            })),
            window,
            cx,
        );
    };

    let initial_custom_instructions = current.custom_instructions.clone();
    let custom_instructions = window.use_keyed_state(
        "settings-general-custom-instructions",
        cx,
        move |window, cx| {
            let mut field = TextInput::new(window, cx)
                .multi_line(8, 24)
                .placeholder("输入全局自定义指令");
            field.set_text(initial_custom_instructions.clone(), cx);
            field
        },
    );
    let custom_instructions_count = custom_instructions.read(cx).text().chars().count();
    let custom_instructions_field = custom_instructions.clone();
    let custom_instructions_dispatch = dispatch.clone();
    let save_custom_instructions =
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
            custom_instructions_dispatch(
                SettingsCommand::SaveCustomInstructions {
                    instructions: custom_instructions_field.read(cx).text().to_owned(),
                },
                window,
                cx,
            );
        };
    let about = about_section(current.app_update.as_ref(), dispatch);

    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            SettingsSection::new("终端")
                .without_header()
                .row(
                    SettingsRow::new("继承系统终端 Profile")
                        .description(
                            "新建终端时控制 Shell 是否加载登录 profile；关闭时仍保留基础环境。",
                        )
                        .control(terminal_profile),
                )
                .row(
                    SettingsRow::new("终端字体")
                        .description(
                            "新建 KeenCode 终端使用的显示字体；留空恢复平台默认字体。",
                        )
                        .detail(
                            div()
                                .w(px(520.0))
                                .max_w_full()
                                .child(
                                    Input::new(&terminal_font)
                                        .size(ControlSize::Lg)
                                        .style(settings_input_style(cx.theme(), true, false)),
                                ),
                        )
                        .control(
                            Button::new("settings-general-terminal-font-apply", "保存")
                                .size(ControlSize::Lg)
                                .on_click(terminal_font_change),
                        ),
                )
                .row(
                    SettingsRow::new("集成终端Shell")
                        .description(
                            "仅新会话生效；控制新建终端使用的 Shell。自动按平台与可用环境选择。",
                        )
                        .control(div().w(px(260.)).max_w_full().child(terminal_shell)),
                )
        )
        .child(
            SettingsSection::new("网络")
                .without_header()
                .row(
                    SettingsRow::new("HTTP 代理")
                        .wide()
                        .description(
                            "模型、MCP、命令工具与应用渲染层的出口流量将经此代理；留空直连。修改后重启应用生效。",
                        )
                        .detail(
                            div()
                                .w(px(520.0))
                                .max_w_full()
                                .child(
                                    Input::new(&http_proxy)
                                        .size(ControlSize::Lg)
                                        .style(settings_input_style(cx.theme(), true, false)),
                                ),
                        )
                        .control(
                            Button::new("settings-general-proxy-save", "保存")
                                .aria_label("保存 HTTP 代理")
                                .size(ControlSize::Lg)
                                .on_click(save_proxy.clone()),
                        ),
                )
                .row(
                    SettingsRow::new("HTTP 代理绕过规则")
                        .wide()
                        .description(
                            "匹配这些主机的请求将直连，不经过 HTTP 代理；多个规则用英文逗号分隔。修改后重启应用生效。",
                        )
                        .detail(
                            div()
                                .w(px(520.0))
                                .max_w_full()
                                .child(
                                    Input::new(&http_proxy_no_proxy)
                                        .size(ControlSize::Lg)
                                        .style(settings_input_style(cx.theme(), true, false)),
                                ),
                        )
                        .control(
                            Button::new("settings-general-proxy-no-proxy-save", "保存")
                                .aria_label("保存 HTTP 代理绕过规则")
                                .size(ControlSize::Lg)
                                .on_click(save_proxy),
                        ),
                )
        )
        .child(
            SettingsSection::new("项目与更新")
                .without_header()
                .row(
                    SettingsRow::new("默认项目目录")
                        .wide()
                        .description("新建项目未指定目录时使用；留空恢复平台默认的 KeenCode 目录。")
                        .control(
                            div()
                                .flex()
                                .w_full()
                                .min_w_0()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .max_w_full()
                                        .child(
                                            Input::new(&project_directory)
                                                .size(ControlSize::Lg)
                                                .style(settings_input_style(cx.theme(), true, false)),
                                        ),
                                )
                                .child(
                                    Button::new(
                                        "settings-general-project-directory-save",
                                        "保存",
                                    )
                                    .aria_label("保存默认项目目录")
                                    .size(ControlSize::Lg)
                                    .on_click(save_project_directory),
                                ),
                        ),
                )
                .row(
                    SettingsRow::new("更新下载源")
                        .description("下一次检查或下载更新时使用。")
                        .control(div().w(px(220.)).max_w_full().child(update_source)),
                ),
        )
        .child(
            SettingsSection::new("应用行为")
                .without_header()
                .row(
                    SettingsRow::new("任务通知")
                        .description("任务完成或失败时显示桌面通知；修改后后续任务立即使用。")
                        .control(task_notifications),
                )
                .row(
                    SettingsRow::new("通知提示音")
                        .description("桌面通知请求播放系统默认提示音；修改后后续通知立即使用。")
                        .control(notification_sound),
                )
                .row(
                    SettingsRow::new("关闭时驻留托盘")
                        .description("关闭主窗口时隐藏到系统托盘；后续关闭操作立即使用。")
                        .control(close_to_tray),
                )
                .row(
                    SettingsRow::new("保持电脑唤醒")
                        .description("阻止空闲睡眠；修改后重启应用生效。")
                        .control(keep_awake),
                )
                .row(
                    SettingsRow::new("本地记忆")
                        .description("使用本机历史对话生成记忆并注入后续请求；修改后立即启停流水线。")
                        .control(local_memories),
                )
                .row(
                    SettingsRow::new("后台 Agent 并发上限")
                        .description("控制同时运行的后台任务数量。")
                        .control(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(120.))
                                        .max_w_full()
                                        .child(
                                            Input::new(&background_agent_limit)
                                                .size(ControlSize::Lg)
                                                .style(settings_input_style(cx.theme(), true, false)),
                                        )
                                )
                                .child(
                                    Button::new(
                                        "settings-general-background-agent-limit-save",
                                        "保存",
                                    )
                                        .aria_label("保存后台 Agent 并发上限")
                                        .size(ControlSize::Lg)
                                        .on_click(save_background_agent_limit),
                                ),
                        ),
                ),
        )
        .child(
            SettingsSection::new("全局自定义指令")
                .without_header()
                .row(
                    SettingsRow::new("指令内容")
                        .wide()
                        .description("保存后仅影响后续新 Turn；已开始 Turn 的冻结上下文不变。")
                        .control(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .w(px(520.))
                                .child(Input::new(&custom_instructions))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(Caption::new(format!(
                                            "{custom_instructions_count} / {MAX_CUSTOM_INSTRUCTIONS_CHARS} 个 Unicode 字符"
                                        )))
                                        .child(
                                            ely_gpui_component::buttons::Button::new(
                                                "settings-general-custom-instructions-save",
                                                "保存自定义指令",
                                            )
                                            .variant(ButtonVariant::Primary)
                                            .disabled(
                                                custom_instructions_count
                                                    > MAX_CUSTOM_INSTRUCTIONS_CHARS,
                                            )
                                            .on_click(save_custom_instructions),
                                        ),
                                ),
                        ),
                ),
        )
        .child(about)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{terminal_shell_from_value, terminal_shell_value};
    use crate::app_settings::TerminalShell;

    #[test]
    fn terminal_shell_values_keep_the_wire_protocol_separate_from_labels() {
        for shell in [
            TerminalShell::Auto,
            TerminalShell::PowerShell,
            TerminalShell::PowerShell7,
            TerminalShell::GitBash,
            TerminalShell::Cmd,
        ] {
            assert_eq!(
                terminal_shell_from_value(terminal_shell_value(shell)),
                Some(shell)
            );
        }
        assert_eq!(terminal_shell_from_value("PowerShell 7"), None);
    }
}
