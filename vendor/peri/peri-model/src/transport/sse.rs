/// 已完成的 SSE event。`data` 保持 provider 原文，JSON 解码由 adapter 负责。
use crate::{ModelError, ModelResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub(crate) event: Option<String>,
    pub(crate) data: String,
}

/// 字节级 SSE parser，安全保留跨 chunk 的 UTF-8 与未完成行。
pub(crate) struct SseParser {
    pending_bytes: Vec<u8>,
    event: Option<String>,
    data_lines: Vec<String>,
    saw_data_field: bool,
    event_observed: bool,
    done: bool,
}

impl SseParser {
    pub(crate) fn new() -> Self {
        Self {
            pending_bytes: Vec::new(),
            event: None,
            data_lines: Vec::new(),
            saw_data_field: false,
            event_observed: false,
            done: false,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> ModelResult<Vec<SseEvent>> {
        if self.done {
            return Ok(Vec::new());
        }
        self.pending_bytes.extend_from_slice(bytes);
        let Some(complete_end) = self
            .pending_bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|index| index + 1)
        else {
            return Ok(Vec::new());
        };

        let remaining = self.pending_bytes.split_off(complete_end);
        let complete = std::mem::replace(&mut self.pending_bytes, remaining);
        let complete = std::str::from_utf8(&complete)
            .map_err(|_| ModelError::protocol(crate::ProtocolErrorKind::Provider))?;

        let mut events = Vec::new();
        for raw_line in complete.split_inclusive('\n') {
            let line = raw_line
                .strip_suffix('\n')
                .expect("split_inclusive always includes newline")
                .strip_suffix('\r')
                .unwrap_or(raw_line.strip_suffix('\n').expect("newline removed"));
            if line.is_empty() {
                if self.saw_data_field {
                    self.event_observed = true;
                    events.push(SseEvent {
                        event: self.event.take(),
                        data: std::mem::take(&mut self.data_lines).join("\n"),
                    });
                    self.saw_data_field = false;
                } else {
                    self.event = None;
                }
                continue;
            }
            if let Some(value) = line.strip_prefix("data:") {
                let value = value.strip_prefix(' ').unwrap_or(value);
                if value == "[DONE]" {
                    self.event_observed = true;
                    self.done = true;
                    break;
                }
                self.saw_data_field = true;
                self.data_lines.push(value.to_owned());
            } else if let Some(value) = line.strip_prefix("event:") {
                self.event = Some(value.strip_prefix(' ').unwrap_or(value).to_owned());
            }
        }
        Ok(events)
    }

    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// 返回上次查询后是否完成了 provider data event，并清除标记。
    ///
    /// 标记与 `push()` 的解析结果分开，让 `[DONE]` 也能作为真实 provider
    /// 事件边界被观测，同时不把它交给 provider decoder。
    pub(crate) fn take_event_observed(&mut self) -> bool {
        std::mem::take(&mut self.event_observed)
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}
