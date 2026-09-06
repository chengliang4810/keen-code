use keencode_model::ModelError;

/// 一条已经完成边界解析的 Server-Sent Event。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SseFrame {
    /// 可选 `event` 字段。
    pub event: Option<String>,
    /// 按 SSE 规则用换行连接的全部 `data` 字段。
    pub data: String,
}

/// 支持任意字节分块、CRLF 和多行 data 的增量 SSE 解码器。
#[derive(Debug)]
pub(crate) struct SseDecoder {
    buffer: Vec<u8>,
    event: Option<String>,
    data: String,
    has_data: bool,
    /// 是否尚未消费流开头的第一行；只在这里允许剥离一次 UTF-8 BOM。
    at_stream_start: bool,
    /// 上一个字节是裸 CR；下一字节若为 LF 只作为同一个换行符消费。
    pending_cr: bool,
    max_event_bytes: usize,
}

impl SseDecoder {
    /// 创建带单事件字节上限的增量解码器。
    pub fn new(max_event_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            event: None,
            data: String::new(),
            has_data: false,
            at_stream_start: true,
            pending_cr: false,
            max_event_bytes,
        }
    }

    /// 追加一个网络字节分块并返回其中完成的事件。
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, ModelError> {
        let mut frames = Vec::new();
        for byte in chunk {
            if self.pending_cr {
                self.pending_cr = false;
                if *byte == b'\n' {
                    continue;
                }
            }
            match *byte {
                b'\n' => {
                    let line = std::mem::take(&mut self.buffer);
                    self.consume_line(&line, &mut frames)?;
                }
                b'\r' => {
                    let line = std::mem::take(&mut self.buffer);
                    self.consume_line(&line, &mut frames)?;
                    self.pending_cr = true;
                }
                byte => self.extend_buffer(&[byte])?,
            }
        }
        Ok(frames)
    }

    /// 在 HTTP 正文结束时处理最后一个未带换行的字段和未分隔事件。
    pub fn finish(&mut self) -> Result<Vec<SseFrame>, ModelError> {
        let mut frames = Vec::new();
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.consume_line(&line, &mut frames)?;
        }
        if self.event.is_some() || self.has_data {
            frames.push(self.take_frame());
        }
        Ok(frames)
    }

    fn consume_line(&mut self, line: &[u8], frames: &mut Vec<SseFrame>) -> Result<(), ModelError> {
        let line = if self.at_stream_start {
            self.at_stream_start = false;
            line.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(line)
        } else {
            line
        };
        let line = std::str::from_utf8(line).map_err(|error| ModelError::Protocol {
            message: format!("SSE 字段不是有效 UTF-8：{error}"),
        })?;
        if line.is_empty() {
            if self.event.is_some() || self.has_data {
                frames.push(self.take_frame());
            }
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => {
                self.event = (!value.is_empty()).then(|| value.to_owned());
            }
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" | "retry" => {}
            _ => {}
        }
        self.check_size()
    }

    fn take_frame(&mut self) -> SseFrame {
        self.has_data = false;
        SseFrame {
            event: self.event.take(),
            data: std::mem::take(&mut self.data),
        }
    }

    /// 在复制网络分块前先执行溢出与单行上限校验。
    fn extend_buffer(&mut self, bytes: &[u8]) -> Result<(), ModelError> {
        let next_len =
            self.buffer
                .len()
                .checked_add(bytes.len())
                .ok_or_else(|| ModelError::Protocol {
                    message: "SSE 字段长度溢出".to_owned(),
                })?;
        let accumulated = next_len
            .checked_add(self.event.as_ref().map_or(0, String::len))
            .and_then(|length| length.checked_add(self.data.len()))
            .ok_or_else(|| ModelError::Protocol {
                message: "SSE 事件长度溢出".to_owned(),
            })?;
        if accumulated > self.max_event_bytes {
            return Err(self.too_large_error());
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// 校验当前仍在组装的事件正文不会超过配置字节上限。
    fn check_size(&self) -> Result<(), ModelError> {
        let accumulated = self
            .buffer
            .len()
            .checked_add(self.event.as_ref().map_or(0, String::len))
            .and_then(|length| length.checked_add(self.data.len()))
            .ok_or_else(|| ModelError::Protocol {
                message: "SSE 事件长度溢出".to_owned(),
            })?;
        if accumulated > self.max_event_bytes {
            return Err(self.too_large_error());
        }
        Ok(())
    }

    /// 构造不包含服务端正文的稳定超限错误。
    fn too_large_error(&self) -> ModelError {
        ModelError::Protocol {
            message: format!("SSE 事件超过 {} 字节安全上限", self.max_event_bytes),
        }
    }
}
