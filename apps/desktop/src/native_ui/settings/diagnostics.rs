use super::{SettingsCommandHandler, SettingsRow, SettingsSection, contracts::*};
use crate::diagnostics::{
    observability::{CrashRecord, ObservabilitySnapshot, StartupPhase},
    process_resources::ProcessResourceSample,
};
use crate::native_insights::PluginHookDiagnostic;
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    typography::Caption,
};
use gpui::{
    AnyElement, App, InteractiveElement, IntoElement, ParentElement, Role,
    StatefulInteractiveElement, Styled, Window, div,
};

fn format_bytes(value: Option<u64>) -> String {
    let Some(value) = value else {
        return "未知".to_owned();
    };
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut scaled = value as f64;
    let mut unit = 0;
    while scaled >= 1024.0 && unit < UNITS.len() - 1 {
        scaled /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} {}", UNITS[unit])
    } else {
        format!("{scaled:.1} {}", UNITS[unit])
    }
}

fn format_percent(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "未知".to_owned())
}

fn format_timestamp(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "未知时间".to_owned())
}

fn resources_section(resources: &ProcessResourceSample) -> AnyElement {
    SettingsSection::new("当前进程资源")
        .description("只采样当前应用进程；打开诊断页或点击刷新时读取一次。")
        .row(
            SettingsRow::new("CPU")
                .control(div().child(Caption::new(format_percent(resources.cpu_percent)))),
        )
        .row(
            SettingsRow::new("工作集")
                .control(div().child(Caption::new(format_bytes(resources.resident_bytes)))),
        )
        .row(
            SettingsRow::new("私有提交")
                .control(div().child(Caption::new(format_bytes(resources.private_bytes)))),
        )
        .row(
            SettingsRow::new("虚拟内存")
                .control(div().child(Caption::new(format_bytes(resources.virtual_bytes)))),
        )
        .row(
            SettingsRow::new("采样进程数").control(
                div().child(Caption::new(
                    resources
                        .process_count
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "未知".to_owned()),
                )),
            ),
        )
        .into_any_element()
}

fn startup_section(phases: &[StartupPhase]) -> AnyElement {
    let section = phases.iter().rev().take(12).fold(
        SettingsSection::new("启动阶段").description("按时间倒序显示最近记录的启动阶段。"),
        |section, phase| {
            section.row(
                SettingsRow::new(phase.phase.clone())
                    .description(format_timestamp(phase.occurred_at_ms))
                    .control(div().child(Caption::new(format!("耗时 {} ms", phase.elapsed_ms)))),
            )
        },
    );
    section.into_any_element()
}

fn crashes_section(crashes: &[CrashRecord]) -> AnyElement {
    let section = crashes.iter().rev().take(8).fold(
        SettingsSection::new("崩溃记录").description("仅显示经过脱敏和长度限制的摘要。"),
        |section, crash| {
            section.row(
                SettingsRow::new(crash.kind.clone())
                    .description(format!(
                        "{} · {}",
                        format_timestamp(crash.occurred_at_ms),
                        crash.message
                    ))
                    .control(
                        div().child(Caption::new(
                            crash
                                .backtrace
                                .as_ref()
                                .map(|_| "含脱敏堆栈")
                                .unwrap_or("无堆栈"),
                        )),
                    ),
            )
        },
    );
    section.into_any_element()
}

fn observability_section(snapshot: &ObservabilitySnapshot) -> AnyElement {
    let counters = snapshot.counters.iter().take(12).fold(
        SettingsSection::new("可观测性摘要").description(format!(
            "schema {} · 捕获于 {}",
            snapshot.schema,
            format_timestamp(snapshot.captured_at_ms)
        )),
        |section, (name, value)| {
            section.row(
                SettingsRow::new(name.clone())
                    .control(div().child(Caption::new(value.to_string()))),
            )
        },
    );
    counters.into_any_element()
}

fn plugin_hook_diagnostics_section(diagnostics: &[PluginHookDiagnostic]) -> Option<AnyElement> {
    if diagnostics.is_empty() {
        return None;
    }
    let section = diagnostics.iter().fold(
        SettingsSection::new("插件 Hook 诊断")
            .description("以下事件已在插件清单中声明，但当前不会执行；支持的 Hook 不显示在此处。"),
        |section, diagnostic| {
            section.row(
                SettingsRow::new(format!("Hook 事件：{}", diagnostic.event))
                    .description(format!(
                        "插件 {} · 声明存在但当前不会执行",
                        diagnostic.plugin_id
                    ))
                    .control(
                        div()
                            .id((
                                gpui::ElementId::from("plugin-hook-reason"),
                                format!("{}:{}", diagnostic.plugin_id, diagnostic.event),
                            ))
                            // 诊断原因必须同时供屏幕阅读器读取，不能只有可视 Caption。
                            .role(Role::Label)
                            .aria_value(format!("原因：{}", diagnostic.reason))
                            .child(Caption::new(format!("原因：{}", diagnostic.reason))),
                    ),
            )
        },
    );
    Some(section.into_any_element())
}

pub(super) fn render(
    current: &DiagnosticsSettings,
    dispatch: SettingsCommandHandler,
    _window: &mut Window,
    _cx: &mut App,
) -> AnyElement {
    let refresh = dispatch.clone();
    let export = dispatch;
    let actions = SettingsSection::new("诊断操作")
        .description(format!("日志文件：{}", current.snapshot.log_path.display()))
        .row(
            SettingsRow::new("刷新采样").control(
                Button::new("settings-diagnostics-refresh", "刷新")
                    .variant(ButtonVariant::Outline)
                    .on_click(move |_, window, cx| {
                        refresh(SettingsCommand::RefreshDiagnostics, window, cx)
                    }),
            ),
        )
        .row(
            SettingsRow::new("脱敏导出")
                .description("导出只包含本地运行摘要，不包含 Prompt、模型输出、请求头或凭据。")
                .control(
                    Button::new("settings-diagnostics-export", "复制脱敏 JSON")
                        .variant(ButtonVariant::Primary)
                        .on_click(move |_, window, cx| {
                            export(SettingsCommand::ExportDiagnostics, window, cx)
                        }),
                ),
        );

    let mut page = div()
        .flex()
        .flex_col()
        .w_full()
        .gap_3()
        .child(actions)
        .child(resources_section(&current.snapshot.resources))
        .child(observability_section(&current.snapshot.observability))
        .child(startup_section(
            &current.snapshot.observability.startup_phases,
        ))
        .child(crashes_section(&current.snapshot.observability.crashes));
    if let Some(section) =
        plugin_hook_diagnostics_section(&current.snapshot.plugin_hook_diagnostics)
    {
        page = page.child(section);
    }
    page.into_any_element()
}
