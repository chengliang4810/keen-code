use super::{SettingsCommandHandler, SettingsRow, SettingsSection, contracts::*};
use crate::analytics::{DailyUsageStat, ModelUsageStat, RequestRecord, RequestRecordsQuery};
use chrono::{Duration, Local, LocalResult, NaiveDate, NaiveTime, TimeZone};
use ely_gpui_component::{
    buttons::{Button, ButtonVariant},
    forms::{Input, TextInput},
    typography::Caption,
};
use gpui::{
    AnyElement, App, IntoElement, ParentElement, Styled, Window, div, prelude::FluentBuilder, px,
};

fn text_field(
    window: &mut Window,
    cx: &mut App,
    key: impl Into<gpui::ElementId>,
    value: String,
    placeholder: &'static str,
) -> gpui::Entity<TextInput> {
    window.use_keyed_state(key, cx, move |window, cx| {
        let mut field = TextInput::new(window, cx).placeholder(placeholder);
        field.set_text(value, cx);
        field
    })
}

/// 将日期编辑器转换为本地时区的闭区间边界；空值才代表不设置该端筛选。
///
/// 结束日期使用次日 00:00 减 1ms，避免漏掉结束日最后 999ms。夏令时发生在日界
/// 时，起始端取较早候选、结束端取较晚候选；完全不存在的本地日界直接返回校验错误。
fn date_to_ms(value: &str, end: bool) -> Result<Option<u64>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| "日期必须使用 YYYY-MM-DD 格式".to_owned())?;
    let boundary_date = if end {
        date.checked_add_signed(Duration::days(1))
            .ok_or_else(|| "结束日期超出可用日期范围".to_owned())?
    } else {
        date
    };
    let midnight = NaiveTime::from_hms_opt(0, 0, 0).expect("00:00:00 必须是有效的本地日界时间");
    let local = Local.from_local_datetime(&boundary_date.and_time(midnight));
    let timestamp = match local {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(earliest, latest) => {
            if end {
                latest
            } else {
                earliest
            }
        }
        LocalResult::None => return Err("该日期在当前本地时区不存在有效日界".to_owned()),
    };
    let timestamp_ms = timestamp.timestamp_millis();
    let timestamp_ms = if end {
        timestamp_ms
            .checked_sub(1)
            .ok_or_else(|| "结束日期超出时间戳范围".to_owned())?
    } else {
        timestamp_ms
    };
    u64::try_from(timestamp_ms)
        .map(Some)
        .map_err(|_| "日期必须不早于 1970-01-01".to_owned())
}

fn date_validation_error(from: &str, to: &str) -> Option<String> {
    let from_ms = match date_to_ms(from, false) {
        Ok(value) => value,
        Err(error) => return Some(format!("起始日期：{error}")),
    };
    let to_ms = match date_to_ms(to, true) {
        Ok(value) => value,
        Err(error) => return Some(format!("结束日期：{error}")),
    };
    if from_ms.zip(to_ms).is_some_and(|(from, to)| from > to) {
        return Some("起始日期不能晚于结束日期".to_owned());
    }
    None
}

fn format_tokens(value: u64) -> String {
    const UNITS: [&str; 4] = ["", "K", "M", "B"];
    let mut scaled = value as f64;
    let mut unit = 0;
    while scaled >= 1_000.0 && unit < UNITS.len() - 1 {
        scaled /= 1_000.0;
        unit += 1;
    }
    if unit == 0 {
        value.to_string()
    } else {
        format!("{scaled:.1} {}", UNITS[unit])
    }
}

fn token_summary(reported_tokens: u64, unreported_requests: u64) -> String {
    if unreported_requests == 0 {
        format!("已报告 {} Token", format_tokens(reported_tokens))
    } else {
        format!(
            "已报告 {} Token；{} 次用量未知",
            format_tokens(reported_tokens),
            unreported_requests
        )
    }
}

fn format_time(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "未知时间".to_owned())
}

fn record_summary(record: &RequestRecord) -> String {
    let duration = format!("{} ms", record.duration_ms);
    let tokens = if record.usage_reported {
        format!(
            "输入 {} · 输出 {}",
            format_tokens(record.input_tokens),
            format_tokens(record.output_tokens)
        )
    } else {
        "Provider 未报告 Token".to_owned()
    };
    format!(
        "{} · {} · {} · {}",
        record.status,
        format_time(record.requested_at_ms),
        duration,
        tokens
    )
}

fn model_section(items: &[ModelUsageStat]) -> AnyElement {
    let section = items.iter().fold(
        SettingsSection::new("按模型汇总")
            .description("仅统计已完成的模型请求；失败和取消请求不会增加 Token 总量。"),
        |section, item| {
            section.row(
                SettingsRow::new(item.model.clone())
                    .description(format!(
                        "{} 次请求 · {}",
                        item.requests,
                        token_summary(item.total_tokens, item.unreported_requests)
                    ))
                    .control(
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .child(Caption::new(format!(
                                "已报告输入 {}",
                                format_tokens(item.input_tokens)
                            )))
                            .child(Caption::new(format!(
                                "已报告输出 {}",
                                format_tokens(item.output_tokens)
                            ))),
                    ),
            )
        },
    );
    section.into_any_element()
}

fn daily_section(items: &[DailyUsageStat]) -> AnyElement {
    let section = items.iter().rev().take(14).fold(
        SettingsSection::new("按日期汇总").description("显示最近 14 个有请求记录的日期。"),
        |section, item| {
            section.row(
                SettingsRow::new(item.date.clone())
                    .description(format!(
                        "{} 次请求 · {}",
                        item.requests,
                        token_summary(item.total_tokens, item.unreported_requests)
                    ))
                    .control(
                        div().child(Caption::new(format!("{} 个模型", item.model_tokens.len()))),
                    ),
            )
        },
    );
    section.into_any_element()
}

fn records_section(records: &[RequestRecord], total: usize) -> AnyElement {
    let section = records.iter().fold(
        SettingsSection::new("请求记录").description(format!(
            "当前页 {} 条，共筛选出 {} 条；记录仅来自本地脱敏请求日志。",
            records.len(),
            total
        )),
        |section, record| {
            let model = if record.provider.is_empty() {
                record.model.clone()
            } else {
                format!("{} · {}", record.provider, record.model)
            };
            section.row(
                SettingsRow::new(model)
                    .description(record_summary(record))
                    .control(div().child(Caption::new(record.request_mode.clone()))),
            )
        },
    );
    section.into_any_element()
}

pub(super) fn render(
    current: &UsageSettings,
    dispatch: SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let query = &current.query;
    let model = text_field(
        window,
        cx,
        "settings-usage-model",
        query.model.clone().unwrap_or_default(),
        "模型标识（可选）",
    );
    let status = text_field(
        window,
        cx,
        "settings-usage-status",
        query.status.clone().unwrap_or_default(),
        "状态，例如 success",
    );
    let from = text_field(
        window,
        cx,
        "settings-usage-from",
        query
            .from_ms
            .map(format_time)
            .unwrap_or_default()
            .get(..10)
            .unwrap_or_default()
            .to_owned(),
        "起始日期 YYYY-MM-DD",
    );
    let to = text_field(
        window,
        cx,
        "settings-usage-to",
        query
            .to_ms
            .map(format_time)
            .unwrap_or_default()
            .get(..10)
            .unwrap_or_default()
            .to_owned(),
        "结束日期 YYYY-MM-DD",
    );
    let from_value = from.read(cx).text().to_owned();
    let to_value = to.read(cx).text().to_owned();
    let date_error = date_validation_error(&from_value, &to_value);

    let apply_dispatch = dispatch.clone();
    let model_input = model.clone();
    let status_input = status.clone();
    let from_input = from.clone();
    let to_input = to.clone();
    let apply = Button::new("settings-usage-apply", "筛选")
        .variant(ButtonVariant::Primary)
        .on_click(move |_, window, cx| {
            let mut next = RequestRecordsQuery::default();
            let model_value = model_input.read(cx).text().trim().to_owned();
            let status_value = status_input.read(cx).text().trim().to_owned();
            let from_value = from_input.read(cx).text().trim().to_owned();
            let to_value = to_input.read(cx).text().trim().to_owned();
            let Ok(from_ms) = date_to_ms(&from_value, false) else {
                return;
            };
            let Ok(to_ms) = date_to_ms(&to_value, true) else {
                return;
            };
            if from_ms.zip(to_ms).is_some_and(|(from, to)| from > to) {
                return;
            }
            next.model = (!model_value.is_empty()).then_some(model_value);
            next.status = (!status_value.is_empty()).then_some(status_value);
            next.from_ms = from_ms;
            next.to_ms = to_ms;
            apply_dispatch(SettingsCommand::QueryUsage(next), window, cx);
        });
    let apply = apply.disabled(date_error.is_some());

    let previous_dispatch = dispatch.clone();
    let mut previous_query = query.clone();
    previous_query.offset = Some(
        query
            .offset
            .unwrap_or(0)
            .saturating_sub(query.limit.unwrap_or(20)),
    );
    let previous = Button::new("settings-usage-previous", "上一页")
        .variant(ButtonVariant::Outline)
        .disabled(query.offset.unwrap_or(0) == 0)
        .on_click(move |_, window, cx| {
            previous_dispatch(
                SettingsCommand::QueryUsage(previous_query.clone()),
                window,
                cx,
            )
        });

    let next_dispatch = dispatch.clone();
    let mut next_query = query.clone();
    next_query.offset = Some(
        query
            .offset
            .unwrap_or(0)
            .saturating_add(query.limit.unwrap_or(20)),
    );
    let next = Button::new("settings-usage-next", "下一页")
        .variant(ButtonVariant::Outline)
        .disabled(!current.snapshot.records.has_more)
        .on_click(move |_, window, cx| {
            next_dispatch(SettingsCommand::QueryUsage(next_query.clone()), window, cx)
        });

    let stats = &current.snapshot.stats;
    let cache = current.snapshot.task_cache.as_ref().map(|value| {
        value
            .cache_hit_rate
            .map(|rate| format!("{:.1}%", rate * 100.0))
            .unwrap_or_else(|| "未知".to_owned())
    });
    let filters = SettingsSection::new("筛选与分页")
        .description(
            "日期按本地请求日志的 Unix 时间筛选；查询和分页均由 NativeInsights 在后台执行。",
        )
        .row(
            SettingsRow::new("模型")
                .control(div().w(px(300.)).max_w_full().child(Input::new(&model))),
        )
        .row(
            SettingsRow::new("状态")
                .control(div().w(px(240.)).max_w_full().child(Input::new(&status))),
        )
        .row(
            SettingsRow::new("日期范围").control(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(div().w(px(190.)).max_w_full().child(Input::new(&from)))
                            .child(div().w(px(190.)).max_w_full().child(Input::new(&to))),
                    )
                    .when_some(date_error, |view, error| view.child(Caption::new(error))),
            ),
        )
        .row(
            SettingsRow::new("操作").control(
                div()
                    .flex()
                    .gap_2()
                    .child(apply)
                    .child(previous)
                    .child(next),
            ),
        );

    let overview = SettingsSection::new("总览")
        .description(
            "统计范围为本地请求记录文件最近可读取的有界窗口；未报告 usage 的请求不计入 Token。",
        )
        .row(
            SettingsRow::new("请求次数")
                .control(div().child(Caption::new(stats.total_requests.to_string()))),
        )
        .row(
            SettingsRow::new("Token 总量").control(div().child(Caption::new(token_summary(
                stats.total_tokens,
                stats.unreported_requests,
            )))),
        )
        .row(
            SettingsRow::new("当前任务缓存命中")
                .description("只统计当前聚焦任务中仍保留且成功的主 Agent 请求。")
                .control(div().child(Caption::new(cache.unwrap_or_else(|| "暂无任务".to_owned())))),
        );

    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_3()
        .child(filters)
        .child(overview)
        .child(model_section(&stats.models))
        .child(daily_section(&stats.days))
        .child(records_section(
            &current.snapshot.records.records,
            current.snapshot.records.total,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{date_to_ms, date_validation_error, token_summary};

    #[test]
    fn date_filter_rejects_invalid_text_and_empty_is_unfiltered() {
        assert_eq!(date_to_ms("", false).unwrap(), None);
        assert!(date_to_ms("2026/10/05", false).is_err());
        assert!(date_validation_error("2026-10-06", "2026-10-05").is_some());
    }

    #[test]
    fn date_filter_end_includes_the_last_millisecond_of_the_local_day() {
        let start = date_to_ms("2026-10-05", false).unwrap().unwrap();
        let end = date_to_ms("2026-10-05", true).unwrap().unwrap();
        assert!(end >= start);
        assert_eq!(end % 1_000, 999);
    }

    #[test]
    fn token_summary_keeps_unreported_usage_unknown() {
        assert_eq!(token_summary(0, 2), "已报告 0 Token；2 次用量未知");
        assert_eq!(token_summary(12, 0), "已报告 12 Token");
    }
}
