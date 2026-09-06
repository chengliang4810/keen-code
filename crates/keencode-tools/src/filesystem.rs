//! UTF-8 文件读取、精确编辑与同目录原子写入工具。

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use keencode_agent::{
    AgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
};
use keencode_model::{ImageContent, ToolDefinition, ToolResultContent};
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::NamedTempFile;

use crate::environment::{ToolEnvironment, display_path, invalid_input};

/// 未指定 `limit` 时单次读取的默认行数。
const DEFAULT_READ_LINES: usize = 2_000;

/// 文本与图片同步读取每次最多从操作系统接收的字节数。
const READ_BUFFER_BYTES: usize = 8 * 1024;

/// 空文件或空选择范围采用的稳定正文。
const EMPTY_READ_BODY: &str = "<空文件或所选范围没有内容>";

/// 无法让当前请求取得至少一整行进展时采用的稳定错误说明。
const READ_LINE_TOO_LARGE_MESSAGE: &str =
    "单行内容无法在 Read 输出字节上限内与必要的文件头和续读提示一起完整返回";

/// 按一基行号读取 UTF-8 文本或内联返回受支持图片的工具。
pub struct ReadTool {
    /// 当前 Session 的工作目录与资源上限。
    environment: Arc<ToolEnvironment>,
}

impl ReadTool {
    /// 创建一个绑定到指定 Session 环境的读取工具。
    pub fn new(environment: Arc<ToolEnvironment>) -> Self {
        Self { environment }
    }
}

impl AgentTool for ReadTool {
    /// 返回读取路径、起始行和行数的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Read",
            "读取 UTF-8 文本文件并返回带一基行号的内容；可用 offset 和 limit 分段。PNG、JPEG、GIF、WebP 图片会作为内联图片返回。",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "minLength": 1 },
                    "offset": { "type": "integer", "minimum": 1 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["file_path"],
                "additionalProperties": false
            }),
        )
    }

    /// 读取文件不会改变外部状态。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_read_input(input, self.environment.limits().max_read_lines)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 不同文件读取可以与其他只读工具并发。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 在阻塞线程中读取文件并持续观察 Turn 取消令牌。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let environment = self.environment.clone();
        Box::pin(async move {
            let input = parse_read_input(&input, environment.limits().max_read_lines)?;
            let cancellation = context.cancellation;
            tokio::task::spawn_blocking(move || read_file(&environment, &cancellation, input))
                .await
                .map_err(join_error)?
        })
    }
}

/// 使用精确旧文本匹配执行单次或全部替换的工具。
pub struct EditTool {
    /// 当前 Session 的工作目录与资源上限。
    environment: Arc<ToolEnvironment>,
}

impl EditTool {
    /// 创建一个绑定到指定 Session 环境的精确编辑工具。
    pub fn new(environment: Arc<ToolEnvironment>) -> Self {
        Self { environment }
    }
}

impl AgentTool for EditTool {
    /// 返回精确旧文本、新文本和全量替换开关的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Edit",
            "在 UTF-8 文件中精确替换 old_string。默认要求只匹配一次；replace_all=true 时替换全部非重叠匹配。写入采用同目录原子替换并保留 UTF-8 BOM 和原文件权限。",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "minLength": 1 },
                    "old_string": { "type": "string", "minLength": 1 },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean", "default": false }
                },
                "required": ["file_path", "old_string", "new_string"],
                "additionalProperties": false
            }),
        )
    }

    /// 精确编辑可能改变文件内容，因此必须受 Plan 只读边界约束。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_edit_input(input)?;
        Ok(ToolEffect::ChangesState)
    }

    /// 文件编辑必须作为顺序副作用屏障执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 在阻塞线程中完成精确匹配和原子替换。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let environment = self.environment.clone();
        Box::pin(async move {
            let input = parse_edit_input(&input)?;
            tokio::task::spawn_blocking(move || edit_file(&environment, &context, input))
                .await
                .map_err(join_error)?
        })
    }
}

/// 创建或完整覆盖一个 UTF-8 文件的原子写入工具。
pub struct WriteTool {
    /// 当前 Session 的工作目录与资源上限。
    environment: Arc<ToolEnvironment>,
}

impl WriteTool {
    /// 创建一个绑定到指定 Session 环境的写入工具。
    pub fn new(environment: Arc<ToolEnvironment>) -> Self {
        Self { environment }
    }
}

impl AgentTool for WriteTool {
    /// 返回目标路径和完整 UTF-8 内容的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Write",
            "创建或完整覆盖 UTF-8 文件。缺失的父目录会随本次调用创建；文件内容在目标目录中完成临时写入和同步后原子替换。",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "minLength": 1 },
                    "content": { "type": "string" }
                },
                "required": ["file_path", "content"],
                "additionalProperties": false
            }),
        )
    }

    /// 完整写入可能创建目录并改变文件，因此必须受 Plan 只读边界约束。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_write_input(input)?;
        Ok(ToolEffect::ChangesState)
    }

    /// 文件写入必须作为顺序副作用屏障执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 在阻塞线程中创建父目录并原子持久化完整内容。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let environment = self.environment.clone();
        Box::pin(async move {
            let input = parse_write_input(&input)?;
            tokio::task::spawn_blocking(move || write_file(&environment, &context, input))
                .await
                .map_err(join_error)?
        })
    }
}

/// `Read` 的严格反序列化输入。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    /// 绝对路径或相对 Session 工作目录的路径。
    file_path: String,
    /// 一基起始行；省略时从第一行开始。
    offset: Option<usize>,
    /// 最多返回的行数；省略时使用默认值。
    limit: Option<usize>,
}

/// `Edit` 的严格反序列化输入。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    /// 绝对路径或相对 Session 工作目录的路径。
    file_path: String,
    /// 必须在原文件中完整出现的非空文本。
    old_string: String,
    /// 替换后的完整文本，可为空。
    new_string: String,
    /// 是否替换全部非重叠匹配。
    #[serde(default)]
    replace_all: bool,
}

/// `Write` 的严格反序列化输入。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    /// 绝对路径或相对 Session 工作目录的路径。
    file_path: String,
    /// 要完整写入的 UTF-8 内容。
    content: String,
}

/// 解析并校验读取输入及其行数上限。
fn parse_read_input(input: &Value, maximum_lines: usize) -> Result<ReadInput, ToolError> {
    let input: ReadInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    if input.file_path.trim().is_empty() {
        return Err(ToolError::permanent("invalid_path", "读取路径不能为空"));
    }
    if input.offset == Some(0) {
        return Err(ToolError::permanent(
            "invalid_offset",
            "offset 必须从 1 开始",
        ));
    }
    if matches!(input.limit, Some(0)) {
        return Err(ToolError::permanent("invalid_limit", "limit 必须大于零"));
    }
    if input.limit.is_some_and(|limit| limit > maximum_lines) {
        return Err(ToolError::permanent(
            "read_limit_exceeded",
            format!("limit 不能超过 {maximum_lines} 行"),
        ));
    }
    Ok(input)
}

/// 解析并校验精确编辑输入。
fn parse_edit_input(input: &Value) -> Result<EditInput, ToolError> {
    let input: EditInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    if input.file_path.trim().is_empty() {
        return Err(ToolError::permanent("invalid_path", "编辑路径不能为空"));
    }
    if input.old_string.is_empty() {
        return Err(ToolError::permanent(
            "empty_old_string",
            "old_string 不能为空",
        ));
    }
    Ok(input)
}

/// 解析并校验完整写入输入。
fn parse_write_input(input: &Value) -> Result<WriteInput, ToolError> {
    let input: WriteInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    if input.file_path.trim().is_empty() {
        return Err(ToolError::permanent("invalid_path", "写入路径不能为空"));
    }
    Ok(input)
}

/// 根据文件类型读取文本行或内联图片。
fn read_file(
    environment: &ToolEnvironment,
    cancellation: &keencode_agent::TurnCancellation,
    input: ReadInput,
) -> Result<ToolOutput, ToolError> {
    ensure_not_cancelled(cancellation)?;
    let path = environment.resolve_path(&input.file_path)?;
    let metadata =
        fs::metadata(&path).map_err(|error| io_error("read_metadata_failed", &path, error))?;
    if !metadata.is_file() {
        return Err(ToolError::permanent(
            "not_a_file",
            format!("读取目标不是普通文件：{}", display_path(&path)),
        ));
    }
    if let Some(media_type) = image_media_type(&path) {
        return read_image(environment, cancellation, &path, media_type, metadata.len());
    }

    let offset = input.offset.unwrap_or(1);
    let limit = input
        .limit
        .unwrap_or_else(|| DEFAULT_READ_LINES.min(environment.limits().max_read_lines));
    read_text_lines(
        cancellation,
        &path,
        offset,
        limit,
        environment.limits().max_read_output_bytes,
    )
}

/// 读取受支持图片并编码为 Provider 中立工具结果。
fn read_image(
    environment: &ToolEnvironment,
    cancellation: &keencode_agent::TurnCancellation,
    path: &Path,
    media_type: &'static str,
    file_bytes: u64,
) -> Result<ToolOutput, ToolError> {
    if file_bytes > environment.limits().max_image_bytes {
        return Err(ToolError::permanent(
            "image_too_large",
            format!(
                "图片大小 {file_bytes} 字节，超过上限 {} 字节",
                environment.limits().max_image_bytes
            ),
        ));
    }
    ensure_not_cancelled(cancellation)?;
    let bytes = read_bounded_image_bytes(cancellation, path, environment.limits().max_image_bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > environment.limits().max_image_bytes {
        return Err(ToolError::permanent(
            "image_too_large",
            format!(
                "图片读取后超过上限 {} 字节",
                environment.limits().max_image_bytes
            ),
        ));
    }
    validate_image_signature(media_type, &bytes)?;
    ensure_not_cancelled(cancellation)?;
    Ok(ToolOutput {
        content: vec![
            ToolResultContent::Text {
                text: format!("图片：{}（{} 字节）", display_path(path), bytes.len()),
            },
            ToolResultContent::Image {
                image: ImageContent::from_base64(media_type, BASE64_STANDARD.encode(bytes)),
            },
        ],
    })
}

/// 以“上限加一”的有界窗口读取图片，并在每个固定大小块之间观察取消。
fn read_bounded_image_bytes(
    cancellation: &keencode_agent::TurnCancellation,
    path: &Path,
    maximum_bytes: u64,
) -> Result<Vec<u8>, ToolError> {
    let mut file = File::open(path).map_err(|error| io_error("read_failed", path, error))?;
    let read_ceiling = maximum_bytes.saturating_add(1);
    let initial_capacity =
        usize::try_from(maximum_bytes.min(READ_BUFFER_BYTES as u64)).unwrap_or(READ_BUFFER_BYTES);
    let mut bytes = Vec::with_capacity(initial_capacity);
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    let mut total_read = 0_u64;

    while total_read < read_ceiling {
        ensure_not_cancelled(cancellation)?;
        let remaining = read_ceiling.saturating_sub(total_read);
        let chunk_limit =
            usize::try_from(remaining.min(READ_BUFFER_BYTES as u64)).unwrap_or(READ_BUFFER_BYTES);
        let count = file
            .read(&mut buffer[..chunk_limit])
            .map_err(|error| io_error("read_failed", path, error))?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        total_read = total_read.saturating_add(count as u64);
        ensure_not_cancelled(cancellation)?;
    }
    Ok(bytes)
}

/// 流式读取指定文本行，避免为了局部预览加载完整文件。
fn read_text_lines(
    cancellation: &keencode_agent::TurnCancellation,
    path: &Path,
    offset: usize,
    limit: usize,
    maximum_output_bytes: usize,
) -> Result<ToolOutput, ToolError> {
    let file = File::open(path).map_err(|error| io_error("read_failed", path, error))?;
    let mut reader = BufReader::with_capacity(READ_BUFFER_BYTES, file);
    let displayed_path = display_path(path);
    let header_bytes = "文件："
        .len()
        .checked_add(displayed_path.len())
        .and_then(|length| length.checked_add(1))
        .ok_or_else(read_output_limit_too_small)?;
    if header_bytes > maximum_output_bytes {
        return Err(read_output_limit_too_small());
    }
    let mut output = String::with_capacity(maximum_output_bytes.min(READ_BUFFER_BYTES));
    output.push_str("文件：");
    output.push_str(&displayed_path);
    output.push('\n');

    let mut line_number = 0_usize;
    let lines_to_skip = offset.saturating_sub(1);
    while line_number < lines_to_skip {
        if !skip_utf8_line(&mut reader, cancellation, path)? {
            break;
        }
        line_number = next_line_number(line_number)?;
    }
    if line_number < lines_to_skip {
        return Err(ToolError::permanent(
            "offset_out_of_range",
            format!("offset {offset} 超出文件末尾；文件共 {line_number} 行"),
        ));
    }

    let mut rendered_boundaries = Vec::new();
    while rendered_boundaries.len() < limit {
        let current_line_number = next_line_number(line_number)?;
        let line = match read_bounded_visible_line(
            &mut reader,
            cancellation,
            path,
            maximum_output_bytes,
            current_line_number == 1,
        ) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error)
                if error.code == "read_line_too_large" && !rendered_boundaries.is_empty() =>
            {
                return finalize_continuation(
                    output,
                    &mut rendered_boundaries,
                    current_line_number,
                    maximum_output_bytes,
                );
            }
            Err(error) => return Err(error),
        };
        line_number = current_line_number;
        let prefix = format!("{line_number:>6}→");
        let separator_bytes = usize::from(!rendered_boundaries.is_empty());
        let rendered_bytes = output
            .len()
            .checked_add(separator_bytes)
            .and_then(|length| length.checked_add(prefix.len()))
            .and_then(|length| length.checked_add(line.len()));
        if rendered_bytes.is_none_or(|length| length > maximum_output_bytes) {
            return finalize_continuation(
                output,
                &mut rendered_boundaries,
                line_number,
                maximum_output_bytes,
            );
        }

        let output_length_before_line = output.len();
        if separator_bytes != 0 {
            output.push('\n');
        }
        output.push_str(&prefix);
        output.push_str(&line);
        rendered_boundaries.push((output_length_before_line, line_number));

        if !reader_has_more(&mut reader, cancellation, path)? {
            return Ok(ToolOutput::text(output));
        }
    }

    if rendered_boundaries.is_empty() {
        let empty_output_bytes = output
            .len()
            .checked_add(EMPTY_READ_BODY.len())
            .ok_or_else(read_output_limit_too_small)?;
        if empty_output_bytes > maximum_output_bytes {
            return Err(read_output_limit_too_small());
        }
        output.push_str(EMPTY_READ_BODY);
        return Ok(ToolOutput::text(output));
    }

    let next_offset = next_line_number(line_number)?;
    finalize_continuation(
        output,
        &mut rendered_boundaries,
        next_offset,
        maximum_output_bytes,
    )
}

/// 在不保存正文的情况下跳过一整行，并以固定缓冲持续校验 UTF-8、NUL 与取消。
fn skip_utf8_line<R: BufRead>(
    reader: &mut R,
    cancellation: &keencode_agent::TurnCancellation,
    path: &Path,
) -> Result<bool, ToolError> {
    let mut pending_utf8 = Vec::with_capacity(4);
    let mut saw_bytes = false;
    let mut contains_nul = false;
    loop {
        ensure_not_cancelled(cancellation)?;
        let (consumed, reached_line_end) = {
            let available = reader
                .fill_buf()
                .map_err(|error| io_error("invalid_utf8_or_read_failed", path, error))?;
            if available.is_empty() {
                if !saw_bytes {
                    return Ok(false);
                }
                finish_streamed_line_validation(&pending_utf8, contains_nul, path)?;
                return Ok(true);
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |index| index + 1);
            let fragment = &available[..consumed];
            saw_bytes = true;
            contains_nul |= fragment.contains(&0);
            validate_utf8_fragment(&mut pending_utf8, fragment, path)?;
            (consumed, newline.is_some())
        };
        reader.consume(consumed);
        ensure_not_cancelled(cancellation)?;
        if reached_line_end {
            finish_streamed_line_validation(&pending_utf8, contains_nul, path)?;
            return Ok(true);
        }
    }
}

/// 有界收集一整行的可见字节，完整保留 UTF-8 边界并去除 BOM 与行尾 CRLF。
fn read_bounded_visible_line<R: BufRead>(
    reader: &mut R,
    cancellation: &keencode_agent::TurnCancellation,
    path: &Path,
    maximum_output_bytes: usize,
    strip_bom: bool,
) -> Result<Option<String>, ToolError> {
    let line_buffer_limit = maximum_output_bytes.saturating_add(usize::from(strip_bom) * 3);
    let mut visible = Vec::with_capacity(line_buffer_limit.min(READ_BUFFER_BYTES));
    let mut pending_carriage_returns = 0_usize;
    let mut saw_bytes = false;

    loop {
        ensure_not_cancelled(cancellation)?;
        let (consumed, reached_line_end) = {
            let available = reader
                .fill_buf()
                .map_err(|error| io_error("invalid_utf8_or_read_failed", path, error))?;
            if available.is_empty() {
                if !saw_bytes {
                    return Ok(None);
                }
                break;
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |index| index + 1);
            let content_bytes = newline.map_or(consumed, |_| consumed.saturating_sub(1));
            append_visible_line_bytes(
                &mut visible,
                &mut pending_carriage_returns,
                &available[..content_bytes],
                line_buffer_limit,
            )?;
            saw_bytes = true;
            (consumed, newline.is_some())
        };
        reader.consume(consumed);
        ensure_not_cancelled(cancellation)?;
        if reached_line_end {
            break;
        }
    }

    let mut line = String::from_utf8(visible).map_err(|error| {
        io_error(
            "invalid_utf8_or_read_failed",
            path,
            std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        )
    })?;
    if strip_bom && line.starts_with('\u{feff}') {
        line.drain(..'\u{feff}'.len_utf8());
    }
    if line.contains('\0') {
        return Err(binary_file_error());
    }
    Ok(Some(line))
}

/// 把一段行正文追加到硬上限缓冲，并延迟保存可能属于行尾的回车字节。
fn append_visible_line_bytes(
    visible: &mut Vec<u8>,
    pending_carriage_returns: &mut usize,
    fragment: &[u8],
    line_buffer_limit: usize,
) -> Result<(), ToolError> {
    for byte in fragment {
        if *byte == b'\r' {
            *pending_carriage_returns = pending_carriage_returns
                .checked_add(1)
                .ok_or_else(read_line_too_large)?;
            continue;
        }
        let additional_bytes = pending_carriage_returns
            .checked_add(1)
            .ok_or_else(read_line_too_large)?;
        let next_length = visible
            .len()
            .checked_add(additional_bytes)
            .ok_or_else(read_line_too_large)?;
        if next_length > line_buffer_limit {
            return Err(read_line_too_large());
        }
        visible.extend(std::iter::repeat_n(b'\r', *pending_carriage_returns));
        visible.push(*byte);
        *pending_carriage_returns = 0;
    }
    Ok(())
}

/// 增量校验被跳过行的 UTF-8，并仅保留跨固定读取块的不完整尾部。
fn validate_utf8_fragment(
    pending_utf8: &mut Vec<u8>,
    fragment: &[u8],
    path: &Path,
) -> Result<(), ToolError> {
    pending_utf8.extend_from_slice(fragment);
    match std::str::from_utf8(pending_utf8) {
        Ok(_) => pending_utf8.clear(),
        Err(error) if error.error_len().is_none() => {
            let valid_bytes = error.valid_up_to();
            pending_utf8.drain(..valid_bytes);
        }
        Err(error) => {
            return Err(io_error(
                "invalid_utf8_or_read_failed",
                path,
                std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            ));
        }
    }
    Ok(())
}

/// 在被跳过行结束时拒绝不完整 UTF-8 尾部，并保持既有 NUL 二进制语义。
fn finish_streamed_line_validation(
    pending_utf8: &[u8],
    contains_nul: bool,
    path: &Path,
) -> Result<(), ToolError> {
    if !pending_utf8.is_empty() {
        let error = std::str::from_utf8(pending_utf8).expect_err("非空 UTF-8 尾部应不完整");
        return Err(io_error(
            "invalid_utf8_or_read_failed",
            path,
            std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        ));
    }
    if contains_nul {
        return Err(binary_file_error());
    }
    Ok(())
}

/// 检查当前行后是否还有字节，避免为了续读判断而加载下一整行。
fn reader_has_more<R: BufRead>(
    reader: &mut R,
    cancellation: &keencode_agent::TurnCancellation,
    path: &Path,
) -> Result<bool, ToolError> {
    ensure_not_cancelled(cancellation)?;
    let has_more = !reader
        .fill_buf()
        .map_err(|error| io_error("invalid_utf8_or_read_failed", path, error))?
        .is_empty();
    ensure_not_cancelled(cancellation)?;
    Ok(has_more)
}

/// 回退必要的完整行直到续读提示可放入上限，并保证下一次 offset 指向首个遗漏行。
fn finalize_continuation(
    mut output: String,
    rendered_boundaries: &mut Vec<(usize, usize)>,
    mut next_offset: usize,
    maximum_output_bytes: usize,
) -> Result<ToolOutput, ToolError> {
    loop {
        if rendered_boundaries.is_empty() {
            return Err(read_line_too_large());
        }
        let marker = format!("[仍有后续内容；下一次使用 offset={next_offset}]");
        let completed_bytes = output
            .len()
            .checked_add(1)
            .and_then(|length| length.checked_add(marker.len()));
        if completed_bytes.is_some_and(|length| length <= maximum_output_bytes) {
            output.push('\n');
            output.push_str(&marker);
            return Ok(ToolOutput::text(output));
        }
        let Some((output_length_before_line, removed_line_number)) = rendered_boundaries.pop()
        else {
            return Err(read_line_too_large());
        };
        output.truncate(output_length_before_line);
        next_offset = removed_line_number;
    }
}

/// 生成固定且不可重试的单行过大错误。
fn read_line_too_large() -> ToolError {
    ToolError::permanent("read_line_too_large", READ_LINE_TOO_LARGE_MESSAGE)
}

/// 生成配置字节预算连固定读取元数据也无法容纳时的不可重试错误。
fn read_output_limit_too_small() -> ToolError {
    ToolError::permanent(
        "read_output_limit_too_small",
        "Read 输出字节上限不足以容纳固定文件头或空范围说明",
    )
}

/// 生成与既有文本读取一致的 NUL 二进制文件错误。
fn binary_file_error() -> ToolError {
    ToolError::permanent(
        "binary_file",
        "文本读取检测到 NUL 字节；请使用适合该二进制格式的工具",
    )
}

/// 递增一基行号，并把理论上的整数溢出归一为稳定工具错误。
fn next_line_number(current: usize) -> Result<usize, ToolError> {
    current.checked_add(1).ok_or_else(|| {
        ToolError::permanent(
            "read_line_number_overflow",
            "文本文件行号超出平台可表示范围",
        )
    })
}

/// 精确替换 UTF-8 文件并保留 BOM 与权限。
fn edit_file(
    environment: &ToolEnvironment,
    context: &ToolContext,
    input: EditInput,
) -> Result<ToolOutput, ToolError> {
    let cancellation = &context.cancellation;
    ensure_not_cancelled(cancellation)?;
    let path = environment.resolve_path(&input.file_path)?;
    reject_symbolic_link(&path)?;
    let metadata =
        fs::metadata(&path).map_err(|error| io_error("read_metadata_failed", &path, error))?;
    if !metadata.is_file() {
        return Err(ToolError::permanent(
            "not_a_file",
            format!("编辑目标不是普通文件：{}", display_path(&path)),
        ));
    }
    if metadata.len() > environment.limits().max_mutation_file_bytes {
        return Err(ToolError::permanent(
            "edit_file_too_large",
            format!(
                "文件大小 {} 字节，超过编辑上限 {} 字节",
                metadata.len(),
                environment.limits().max_mutation_file_bytes
            ),
        ));
    }
    let maximum_bytes = environment.limits().max_mutation_file_bytes;
    let original_bytes = read_bounded_file(
        cancellation,
        &path,
        maximum_bytes,
        "read_failed",
        ToolError::permanent(
            "edit_file_too_large",
            format!(
                "文件大小超过编辑上限 {} 字节",
                environment.limits().max_mutation_file_bytes
            ),
        ),
    )?;
    ensure_not_cancelled(cancellation)?;
    let (had_bom, original) = decode_utf8(&original_bytes)?;
    let matches = original.match_indices(&input.old_string).count();
    if matches == 0 {
        return Err(ToolError::permanent(
            "old_string_not_found",
            "old_string 在目标文件中没有精确匹配",
        ));
    }
    if !input.replace_all && matches != 1 {
        return Err(ToolError::permanent(
            "old_string_not_unique",
            format!("old_string 精确匹配 {matches} 次；请扩大上下文或使用 replace_all=true"),
        ));
    }
    let replacement_count = if input.replace_all { matches } else { 1 };
    let edited_size = checked_edit_size(
        original.len(),
        had_bom,
        input.old_string.len(),
        input.new_string.len(),
        replacement_count,
        maximum_bytes,
    )?;
    let edited = if input.replace_all {
        original.replace(&input.old_string, &input.new_string)
    } else {
        original.replacen(&input.old_string, &input.new_string, 1)
    };
    let edited_bytes = encode_utf8(&edited, had_bom);
    debug_assert_eq!(edited_bytes.len(), edited_size);
    if edited_bytes == original_bytes {
        return Ok(ToolOutput::text(format!(
            "文件内容未变化：{}",
            display_path(&path)
        )));
    }
    let prepared = environment
        .file_mutation_recorder()
        .map(|recorder| recorder.prepare(context, &path, Some(&original_bytes), &edited_bytes))
        .transpose()?;
    ensure_not_cancelled(cancellation)?;
    ensure_file_unchanged(&path, &original_bytes, maximum_bytes, cancellation)?;
    ensure_not_cancelled(cancellation)?;
    atomic_write(&path, &edited_bytes, Some(metadata.permissions()))?;
    if let Some(prepared) = prepared {
        prepared.mark_applied()?;
    }
    Ok(ToolOutput::text(format!(
        "已原子编辑 {}，替换 {replacement_count} 处，写入 {} 字节",
        display_path(&path),
        edited_bytes.len()
    )))
}

/// 创建父目录并原子写入完整 UTF-8 内容。
fn write_file(
    environment: &ToolEnvironment,
    context: &ToolContext,
    input: WriteInput,
) -> Result<ToolOutput, ToolError> {
    let cancellation = &context.cancellation;
    ensure_not_cancelled(cancellation)?;
    let path = environment.resolve_path(&input.file_path)?;
    reject_symbolic_link(&path)?;
    let existing = match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Some(metadata),
        Ok(_) => {
            return Err(ToolError::permanent(
                "not_a_file",
                format!("写入目标不是普通文件：{}", display_path(&path)),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_error("write_metadata_failed", &path, error)),
    };
    let content = input.content.into_bytes();
    let content_size = u64::try_from(content.len()).unwrap_or(u64::MAX);
    if content_size > environment.limits().max_mutation_file_bytes {
        return Err(ToolError::permanent(
            "write_content_too_large",
            format!(
                "写入内容大小 {content_size} 字节，超过上限 {} 字节",
                environment.limits().max_mutation_file_bytes
            ),
        ));
    }
    if existing
        .as_ref()
        .is_some_and(|metadata| metadata.len() > environment.limits().max_mutation_file_bytes)
    {
        return Err(ToolError::permanent(
            "write_file_too_large",
            format!(
                "现有文件超过写入上限 {} 字节",
                environment.limits().max_mutation_file_bytes
            ),
        ));
    }
    let maximum_bytes = environment.limits().max_mutation_file_bytes;
    let previous = if existing.is_some() {
        Some(read_bounded_file(
            cancellation,
            &path,
            maximum_bytes,
            "read_failed",
            ToolError::permanent(
                "write_file_too_large",
                format!(
                    "现有文件超过写入上限 {} 字节",
                    environment.limits().max_mutation_file_bytes
                ),
            ),
        )?)
    } else {
        None
    };
    if previous.as_deref() == Some(content.as_slice()) {
        return Ok(ToolOutput::text(format!(
            "文件内容未变化：{}",
            display_path(&path)
        )));
    }
    let parent = path
        .parent()
        .ok_or_else(|| ToolError::permanent("invalid_path", "写入路径没有可用的父目录"))?;
    ensure_not_cancelled(cancellation)?;
    let action = if existing.is_some() {
        "覆盖"
    } else {
        "创建"
    };
    let permissions = existing.map(|metadata| metadata.permissions());
    let prepared = environment
        .file_mutation_recorder()
        .map(|recorder| recorder.prepare(context, &path, previous.as_deref(), &content))
        .transpose()?;
    ensure_not_cancelled(cancellation)?;
    match &previous {
        Some(previous) => ensure_file_unchanged(&path, previous, maximum_bytes, cancellation)?,
        None => ensure_path_still_absent(&path)?,
    }
    ensure_not_cancelled(cancellation)?;
    fs::create_dir_all(parent).map_err(|error| io_error("create_parent_failed", parent, error))?;
    ensure_not_cancelled(cancellation)?;
    atomic_write(&path, &content, permissions)?;
    if let Some(prepared) = prepared {
        prepared.mark_applied()?;
    }
    Ok(ToolOutput::text(format!(
        "已原子{action} {}，写入 {} 字节",
        display_path(&path),
        content.len()
    )))
}

/// 在构造替换结果字符串前计算其完整 UTF-8 字节数并校验文件上限。
fn checked_edit_size(
    original_text_bytes: usize,
    had_bom: bool,
    old_string_bytes: usize,
    new_string_bytes: usize,
    replacement_count: usize,
    maximum_bytes: u64,
) -> Result<usize, ToolError> {
    let removed_bytes = old_string_bytes
        .checked_mul(replacement_count)
        .ok_or_else(|| edit_result_too_large(maximum_bytes))?;
    let retained_bytes = original_text_bytes
        .checked_sub(removed_bytes)
        .ok_or_else(|| edit_result_too_large(maximum_bytes))?;
    let inserted_bytes = new_string_bytes
        .checked_mul(replacement_count)
        .ok_or_else(|| edit_result_too_large(maximum_bytes))?;
    let text_bytes = retained_bytes
        .checked_add(inserted_bytes)
        .ok_or_else(|| edit_result_too_large(maximum_bytes))?;
    let edited_bytes = text_bytes
        .checked_add(if had_bom { 3 } else { 0 })
        .ok_or_else(|| edit_result_too_large(maximum_bytes))?;
    if u64::try_from(edited_bytes).unwrap_or(u64::MAX) > maximum_bytes {
        return Err(edit_result_too_large(maximum_bytes));
    }
    Ok(edited_bytes)
}

/// 生成替换结果超过 Edit 文件上限时的稳定错误。
fn edit_result_too_large(maximum_bytes: u64) -> ToolError {
    ToolError::permanent(
        "edit_file_too_large",
        format!("编辑结果超过上限 {maximum_bytes} 字节"),
    )
}

/// 在目标目录写完并同步临时文件后原子替换目标。
fn atomic_write(
    path: &Path,
    content: &[u8],
    permissions: Option<fs::Permissions>,
) -> Result<(), ToolError> {
    let parent = path
        .parent()
        .ok_or_else(|| ToolError::permanent("invalid_path", "目标文件没有可用的父目录"))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| io_error("create_temp_failed", parent, error))?;
    if let Some(permissions) = permissions {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| io_error("set_permissions_failed", path, error))?;
    }
    temporary
        .write_all(content)
        .map_err(|error| io_error("write_failed", path, error))?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|error| io_error("sync_failed", path, error))?;
    temporary
        .persist(path)
        .map_err(|error| io_error("persist_failed", path, error.error))?;
    Ok(())
}

/// 解码 UTF-8 文本并返回是否存在 BOM。
fn decode_utf8(bytes: &[u8]) -> Result<(bool, &str), ToolError> {
    if bytes.contains(&0) {
        return Err(ToolError::permanent(
            "binary_file",
            "精确编辑不支持包含 NUL 字节的二进制文件",
        ));
    }
    let had_bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    let content = if had_bom { &bytes[3..] } else { bytes };
    let text = std::str::from_utf8(content).map_err(|error| {
        ToolError::permanent("invalid_utf8", format!("文件不是有效 UTF-8：{error}"))
    })?;
    Ok((had_bom, text))
}

/// 按原文件 BOM 状态编码 UTF-8 文本。
fn encode_utf8(text: &str, with_bom: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + usize::from(with_bom) * 3);
    if with_bom {
        bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

/// 按扩展名返回可内联模型的图片媒体类型。
fn image_media_type(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// 校验扩展名声明的常见图片格式具有对应文件签名。
fn validate_image_signature(media_type: &str, bytes: &[u8]) -> Result<(), ToolError> {
    let valid = match media_type {
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ToolError::permanent(
            "invalid_image",
            "文件扩展名与受支持图片格式的文件签名不一致",
        ))
    }
}

/// 拒绝原子替换语义不明确的目标符号链接。
fn reject_symbolic_link(path: &Path) -> Result<(), ToolError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(ToolError::permanent(
            "symbolic_link_write_denied",
            format!("拒绝通过符号链接修改文件：{}", display_path(path)),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("symlink_metadata_failed", path, error)),
    }
}

/// 在提交编辑前重新读取并拒绝覆盖并发产生的文件变化。
fn ensure_file_unchanged(
    path: &Path,
    expected: &[u8],
    maximum_bytes: u64,
    cancellation: &keencode_agent::TurnCancellation,
) -> Result<(), ToolError> {
    let current = read_bounded_file(
        cancellation,
        path,
        maximum_bytes,
        "concurrent_read_failed",
        ToolError::permanent(
            "file_changed_during_tool",
            format!(
                "文件在工具执行期间发生变化，已拒绝覆盖：{}",
                display_path(path)
            ),
        ),
    )?;
    if current != expected {
        return Err(ToolError::permanent(
            "file_changed_during_tool",
            format!(
                "文件在工具执行期间发生变化，已拒绝覆盖：{}",
                display_path(path)
            ),
        ));
    }
    Ok(())
}

/// 以“上限加一”的有界窗口读取待编辑文件，并在每个固定大小块之间观察取消。
fn read_bounded_file(
    cancellation: &keencode_agent::TurnCancellation,
    path: &Path,
    maximum_bytes: u64,
    read_error_code: &str,
    too_large_error: ToolError,
) -> Result<Vec<u8>, ToolError> {
    let mut file = File::open(path).map_err(|error| io_error(read_error_code, path, error))?;
    let read_ceiling = maximum_bytes.saturating_add(1);
    let initial_capacity =
        usize::try_from(maximum_bytes.min(READ_BUFFER_BYTES as u64)).unwrap_or(READ_BUFFER_BYTES);
    let mut bytes = Vec::with_capacity(initial_capacity);
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    let mut total_read = 0_u64;

    while total_read < read_ceiling {
        ensure_not_cancelled(cancellation)?;
        let remaining = read_ceiling.saturating_sub(total_read);
        let chunk_limit =
            usize::try_from(remaining.min(READ_BUFFER_BYTES as u64)).unwrap_or(READ_BUFFER_BYTES);
        let count = file
            .read(&mut buffer[..chunk_limit])
            .map_err(|error| io_error(read_error_code, path, error))?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        total_read = total_read.saturating_add(count as u64);
        ensure_not_cancelled(cancellation)?;
    }
    if total_read > maximum_bytes {
        return Err(too_large_error);
    }
    Ok(bytes)
}

/// 在创建新文件前拒绝覆盖执行期间由其他进程创建的同名路径。
fn ensure_path_still_absent(path: &Path) -> Result<(), ToolError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(ToolError::permanent(
            "file_created_during_tool",
            format!(
                "路径在工具执行期间被创建，已拒绝覆盖：{}",
                display_path(path)
            ),
        )),
        Err(error) => Err(io_error("concurrent_metadata_failed", path, error)),
    }
}

/// 在同步文件操作的安全点响应取消。
fn ensure_not_cancelled(cancellation: &keencode_agent::TurnCancellation) -> Result<(), ToolError> {
    if cancellation.is_cancelled() {
        Err(ToolError::permanent("cancelled", "工具调用已取消"))
    } else {
        Ok(())
    }
}

/// 生成不包含文件内容的稳定 IO 错误。
fn io_error(code: &str, path: &Path, error: std::io::Error) -> ToolError {
    ToolError::permanent(code, format!("{}：{error}", display_path(path)))
}

/// 把 Tokio 阻塞任务异常归一为内部工具错误。
fn join_error(error: tokio::task::JoinError) -> ToolError {
    ToolError::permanent("blocking_task_failed", format!("文件任务异常结束：{error}"))
}
