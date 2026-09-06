//! 遵循 Git 忽略规则的 Glob 与 UTF-8 正则搜索工具。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use globset::{GlobBuilder, GlobMatcher};
use ignore::WalkBuilder;
use keencode_agent::{
    AgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
    TurnCancellation,
};
use keencode_model::ToolDefinition;
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::environment::{ToolEnvironment, display_path, invalid_input};

/// 没有显式指定时单次搜索默认最多返回的结果数量。
const DEFAULT_SEARCH_RESULTS: usize = 1_000;

/// 按 Git 忽略规则遍历并匹配相对路径 Glob 的工具。
pub struct GlobTool {
    /// 当前 Session 的工作目录与资源上限。
    environment: Arc<ToolEnvironment>,
}

impl GlobTool {
    /// 创建一个绑定到指定 Session 环境的 Glob 工具。
    pub fn new(environment: Arc<ToolEnvironment>) -> Self {
        Self { environment }
    }
}

impl AgentTool for GlobTool {
    /// 返回匹配模式、搜索根目录和结果上限的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Glob",
            "在指定目录下按 Git 忽略规则查找文件。pattern 相对搜索根目录并使用 / 作为分隔符；跨目录请显式使用 **。结果按路径排序。",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "minLength": 1 },
                    "path": { "type": "string", "minLength": 1, "default": "." },
                    "max_results": { "type": "integer", "minimum": 1 }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
        )
    }

    /// Glob 只读取目录和文件元数据。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        let parsed = parse_glob_input(input, self.environment.limits().max_search_results)?;
        compile_glob(&parsed.pattern)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 不同目录遍历可以与其他只读工具并发。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 在阻塞线程中执行忽略感知遍历并观察取消令牌。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let environment = self.environment.clone();
        Box::pin(async move {
            let input = parse_glob_input(&input, environment.limits().max_search_results)?;
            let cancellation = context.cancellation;
            tokio::task::spawn_blocking(move || execute_glob(&environment, &cancellation, input))
                .await
                .map_err(join_error)?
        })
    }
}

/// 在忽略感知文件集合中执行 Rust 正则搜索的工具。
pub struct GrepTool {
    /// 当前 Session 的工作目录与资源上限。
    environment: Arc<ToolEnvironment>,
}

impl GrepTool {
    /// 创建一个绑定到指定 Session 环境的 Grep 工具。
    pub fn new(environment: Arc<ToolEnvironment>) -> Self {
        Self { environment }
    }
}

impl AgentTool for GrepTool {
    /// 返回正则、路径过滤、输出方式和上下文行数的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Grep",
            "在 UTF-8 文本文件中执行 Rust 正则搜索并遵循 Git 忽略规则。支持内容、匹配文件和按文件计数三种输出；multiline=true 时 . 可跨换行匹配。",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "minLength": 1 },
                    "path": { "type": "string", "minLength": 1, "default": "." },
                    "glob": { "type": "string", "minLength": 1 },
                    "case_insensitive": { "type": "boolean", "default": false },
                    "multiline": { "type": "boolean", "default": false },
                    "output_mode": {
                        "type": "string",
                        "enum": ["content", "files_with_matches", "count"],
                        "default": "content"
                    },
                    "context_before": { "type": "integer", "minimum": 0, "maximum": 100 },
                    "context_after": { "type": "integer", "minimum": 0, "maximum": 100 },
                    "max_results": { "type": "integer", "minimum": 1 }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
        )
    }

    /// Grep 只读取目录和文本文件。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        let parsed = parse_grep_input(input, self.environment.limits().max_search_results)?;
        compile_regex(&parsed)?;
        if let Some(pattern) = &parsed.glob {
            compile_glob(pattern)?;
        }
        Ok(ToolEffect::ReadOnly)
    }

    /// 不同正则搜索可以与其他只读工具并发。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 在阻塞线程中搜索文件并在文件边界观察取消令牌。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let environment = self.environment.clone();
        Box::pin(async move {
            let input = parse_grep_input(&input, environment.limits().max_search_results)?;
            let cancellation = context.cancellation;
            tokio::task::spawn_blocking(move || execute_grep(&environment, &cancellation, input))
                .await
                .map_err(join_error)?
        })
    }
}

/// `Glob` 的严格反序列化输入。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobInput {
    /// 相对搜索根目录的 Glob 模式。
    pattern: String,
    /// 绝对路径或相对 Session 工作目录的搜索根目录。
    #[serde(default = "default_search_path")]
    path: String,
    /// 本次最多返回的文件数量。
    max_results: Option<usize>,
}

/// `Grep` 支持的输出聚合方式。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum GrepOutputMode {
    /// 返回匹配行及可选上下文。
    #[default]
    Content,
    /// 每个包含匹配的文件只返回一次路径。
    FilesWithMatches,
    /// 返回每个文件的正则匹配次数。
    Count,
}

/// `Grep` 的严格反序列化输入。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepInput {
    /// 由 Rust `regex` 语法解释的非空表达式。
    pattern: String,
    /// 绝对路径或相对 Session 工作目录的文件或目录。
    #[serde(default = "default_search_path")]
    path: String,
    /// 可选的相对路径 Glob 文件过滤器。
    glob: Option<String>,
    /// 是否忽略 Unicode 字符大小写。
    #[serde(default)]
    case_insensitive: bool,
    /// 是否把完整文件作为一段文本并允许跨行匹配。
    #[serde(default)]
    multiline: bool,
    /// 返回内容、文件路径或按文件计数。
    #[serde(default)]
    output_mode: GrepOutputMode,
    /// 内容输出时每个匹配前最多附带的上下文行数。
    #[serde(default)]
    context_before: usize,
    /// 内容输出时每个匹配后最多附带的上下文行数。
    #[serde(default)]
    context_after: usize,
    /// 本次最多返回的匹配行或文件数量。
    max_results: Option<usize>,
}

/// 返回搜索工具默认的当前目录参数。
fn default_search_path() -> String {
    ".".to_owned()
}

/// 解析并校验 Glob 输入及结果上限。
fn parse_glob_input(input: &Value, maximum: usize) -> Result<GlobInput, ToolError> {
    let input: GlobInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    validate_common_search_input(&input.pattern, &input.path, input.max_results, maximum)?;
    Ok(input)
}

/// 解析并校验 Grep 输入、上下文与结果上限。
fn parse_grep_input(input: &Value, maximum: usize) -> Result<GrepInput, ToolError> {
    let input: GrepInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    validate_common_search_input(&input.pattern, &input.path, input.max_results, maximum)?;
    if input
        .glob
        .as_ref()
        .is_some_and(|glob| glob.trim().is_empty())
    {
        return Err(ToolError::permanent("invalid_glob", "glob 过滤器不能为空"));
    }
    if input.context_before > 100 || input.context_after > 100 {
        return Err(ToolError::permanent(
            "context_limit_exceeded",
            "context_before 和 context_after 不能超过 100",
        ));
    }
    if input.output_mode != GrepOutputMode::Content
        && (input.context_before != 0 || input.context_after != 0)
    {
        return Err(ToolError::permanent(
            "invalid_context_mode",
            "上下文行只适用于 output_mode=content",
        ));
    }
    Ok(input)
}

/// 校验搜索模式、根目录和结果数量。
fn validate_common_search_input(
    pattern: &str,
    path: &str,
    requested: Option<usize>,
    maximum: usize,
) -> Result<(), ToolError> {
    if pattern.trim().is_empty() {
        return Err(ToolError::permanent("empty_pattern", "搜索模式不能为空"));
    }
    if path.trim().is_empty() {
        return Err(ToolError::permanent("invalid_path", "搜索路径不能为空"));
    }
    if matches!(requested, Some(0)) {
        return Err(ToolError::permanent(
            "invalid_result_limit",
            "max_results 必须大于零",
        ));
    }
    if requested.is_some_and(|limit| limit > maximum) {
        return Err(ToolError::permanent(
            "result_limit_exceeded",
            format!("max_results 不能超过 {maximum}"),
        ));
    }
    Ok(())
}

/// 编译使用斜杠分隔且 `*` 不跨目录的 Glob。
fn compile_glob(pattern: &str) -> Result<GlobMatcher, ToolError> {
    let normalized = pattern.replace('\\', "/");
    GlobBuilder::new(&normalized)
        .literal_separator(true)
        .backslash_escape(false)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|error| ToolError::permanent("invalid_glob", format!("Glob 无效：{error}")))
}

/// 编译与输入选项一致的 Rust 正则。
fn compile_regex(input: &GrepInput) -> Result<Regex, ToolError> {
    RegexBuilder::new(&input.pattern)
        .case_insensitive(input.case_insensitive)
        .multi_line(input.multiline)
        .dot_matches_new_line(input.multiline)
        .build()
        .map_err(|error| ToolError::permanent("invalid_regex", format!("正则无效：{error}")))
}

/// 执行确定性 Glob 遍历并返回绝对文件路径。
fn execute_glob(
    environment: &ToolEnvironment,
    cancellation: &TurnCancellation,
    input: GlobInput,
) -> Result<ToolOutput, ToolError> {
    ensure_not_cancelled(cancellation)?;
    let root = environment.resolve_path(&input.path)?;
    if !root.is_dir() {
        return Err(ToolError::permanent(
            "not_a_directory",
            format!("Glob 搜索根不是目录：{}", display_path(&root)),
        ));
    }
    let matcher = compile_glob(&input.pattern)?;
    let limit = input
        .max_results
        .unwrap_or_else(|| DEFAULT_SEARCH_RESULTS.min(environment.limits().max_search_results));
    let mut builder = walk_builder(&root);
    builder.sort_by_file_path(|left, right| left.cmp(right));
    let mut matches = Vec::new();
    let mut errors = 0_usize;
    let mut truncated = false;

    for entry in builder.build() {
        ensure_not_cancelled(cancellation)?;
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                errors = errors.saturating_add(1);
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = entry.path().strip_prefix(&root).unwrap_or(entry.path());
        let normalized = display_path(relative);
        if matcher.is_match(&normalized) {
            if matches.len() == limit {
                truncated = true;
                break;
            }
            matches.push(display_path(entry.path()));
        }
    }

    let mut output = if matches.is_empty() {
        "未找到匹配文件".to_owned()
    } else {
        matches.join("\n")
    };
    if truncated {
        output.push_str(&format!("\n[结果已截断到 {limit} 个文件]"));
    }
    if errors != 0 {
        output.push_str(&format!("\n[遍历时跳过 {errors} 个不可读取项]"));
    }
    Ok(ToolOutput::text(output))
}

/// 执行正则搜索并按请求方式聚合结果。
fn execute_grep(
    environment: &ToolEnvironment,
    cancellation: &TurnCancellation,
    input: GrepInput,
) -> Result<ToolOutput, ToolError> {
    ensure_not_cancelled(cancellation)?;
    let root = environment.resolve_path(&input.path)?;
    let metadata = fs::metadata(&root).map_err(|error| {
        ToolError::permanent(
            "search_path_failed",
            format!("{}：{error}", display_path(&root)),
        )
    })?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(ToolError::permanent(
            "invalid_search_path",
            "Grep 搜索路径既不是文件也不是目录",
        ));
    }
    let regex = compile_regex(&input)?;
    let glob = input.glob.as_deref().map(compile_glob).transpose()?;
    let limit = input
        .max_results
        .unwrap_or_else(|| DEFAULT_SEARCH_RESULTS.min(environment.limits().max_search_results));
    let mut files = collect_search_files(&root, cancellation)?;
    files.sort();

    let mut rendered = Vec::new();
    let mut result_count = 0_usize;
    let mut skipped_binary = 0_usize;
    let mut skipped_large = 0_usize;
    let mut skipped_unreadable = 0_usize;
    let mut truncated = false;

    for path in files {
        ensure_not_cancelled(cancellation)?;
        if result_count == limit {
            truncated = true;
            break;
        }
        let relative = relative_for_filter(&root, &path, metadata.is_file());
        if glob
            .as_ref()
            .is_some_and(|matcher| !matcher.is_match(&relative))
        {
            continue;
        }
        let file_metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                continue;
            }
        };
        if file_metadata.len() > environment.limits().max_search_file_bytes {
            skipped_large = skipped_large.saturating_add(1);
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                skipped_unreadable = skipped_unreadable.saturating_add(1);
                continue;
            }
        };
        if bytes.contains(&0) {
            skipped_binary = skipped_binary.saturating_add(1);
            continue;
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text.strip_prefix('\u{feff}').unwrap_or(text),
            Err(_) => {
                skipped_binary = skipped_binary.saturating_add(1);
                continue;
            }
        };
        let analysis = analyze_matches(&regex, text, input.multiline);
        if analysis.match_count == 0 {
            continue;
        }
        let path_display = display_path(&path);
        match input.output_mode {
            GrepOutputMode::Content => {
                let remaining = limit - result_count;
                let selected = analysis
                    .matching_lines
                    .iter()
                    .copied()
                    .take(remaining)
                    .collect::<Vec<_>>();
                if selected.len() < analysis.matching_lines.len() {
                    truncated = true;
                }
                result_count = result_count.saturating_add(selected.len());
                rendered.push(render_content(
                    &path_display,
                    text,
                    &selected,
                    input.context_before,
                    input.context_after,
                ));
            }
            GrepOutputMode::FilesWithMatches => {
                rendered.push(path_display);
                result_count = result_count.saturating_add(1);
            }
            GrepOutputMode::Count => {
                rendered.push(format!("{path_display}:{}", analysis.match_count));
                result_count = result_count.saturating_add(1);
            }
        }
        if truncated {
            break;
        }
    }

    let mut output = if rendered.is_empty() {
        "未找到匹配内容".to_owned()
    } else {
        rendered.join("\n")
    };
    if truncated {
        output.push_str(&format!("\n[结果已截断到 {limit} 项]"));
    }
    if skipped_large != 0 || skipped_binary != 0 || skipped_unreadable != 0 {
        output.push_str(&format!(
            "\n[跳过：超大文件 {skipped_large}，二进制或非 UTF-8 文件 {skipped_binary}，不可读取文件 {skipped_unreadable}]"
        ));
    }
    Ok(ToolOutput::text(output))
}

/// 一个文件内正则匹配次数及涉及的一基行号。
struct MatchAnalysis {
    /// 正则实际匹配次数。
    match_count: usize,
    /// 去重并升序排列的一基匹配行号。
    matching_lines: Vec<usize>,
}

/// 分析单行或跨行正则匹配并映射到一基行号。
fn analyze_matches(regex: &Regex, text: &str, multiline: bool) -> MatchAnalysis {
    if !multiline {
        let mut match_count = 0_usize;
        let mut matching_lines = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let count = regex.find_iter(line).count();
            if count != 0 {
                match_count = match_count.saturating_add(count);
                matching_lines.push(index.saturating_add(1));
            }
        }
        return MatchAnalysis {
            match_count,
            matching_lines,
        };
    }

    let starts = line_starts(text);
    let mut match_count = 0_usize;
    let mut lines = BTreeSet::new();
    for matched in regex.find_iter(text) {
        match_count = match_count.saturating_add(1);
        let start = line_for_offset(&starts, matched.start());
        let end_offset = matched
            .end()
            .saturating_sub(usize::from(!matched.is_empty()));
        let end = line_for_offset(&starts, end_offset);
        for line in start..=end {
            lines.insert(line);
        }
    }
    MatchAnalysis {
        match_count,
        matching_lines: lines.into_iter().collect(),
    }
}

/// 返回文本中每一行的零基字节起点。
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' && index.saturating_add(1) < text.len() {
            starts.push(index.saturating_add(1));
        }
    }
    starts
}

/// 把字节偏移映射为一基行号。
fn line_for_offset(starts: &[usize], offset: usize) -> usize {
    match starts.binary_search(&offset) {
        Ok(index) => index.saturating_add(1),
        Err(index) => index.max(1),
    }
}

/// 渲染匹配行以及去重后的前后上下文。
fn render_content(
    path: &str,
    text: &str,
    matching_lines: &[usize],
    before: usize,
    after: usize,
) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let matched = matching_lines.iter().copied().collect::<BTreeSet<_>>();
    let mut visible = BTreeMap::<usize, bool>::new();
    for &line in matching_lines {
        let start = line.saturating_sub(before).max(1);
        let end = line.saturating_add(after).min(lines.len());
        for current in start..=end {
            visible
                .entry(current)
                .and_modify(|is_match| *is_match |= matched.contains(&current))
                .or_insert_with(|| matched.contains(&current));
        }
    }
    let mut output = format!("{path}\n");
    for (line_number, is_match) in visible {
        let separator = if is_match { ':' } else { '-' };
        let content = lines
            .get(line_number.saturating_sub(1))
            .copied()
            .unwrap_or("");
        output.push_str(&format!("{line_number}{separator}{content}\n"));
    }
    output.pop();
    output
}

/// 收集单个文件或忽略感知目录中的普通文件。
fn collect_search_files(
    root: &Path,
    cancellation: &TurnCancellation,
) -> Result<Vec<PathBuf>, ToolError> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        return Err(ToolError::permanent(
            "invalid_search_path",
            "Grep 搜索路径既不是文件也不是目录",
        ));
    }
    let mut builder = walk_builder(root);
    builder.sort_by_file_path(|left, right| left.cmp(right));
    let mut files = Vec::new();
    for entry in builder.build() {
        ensure_not_cancelled(cancellation)?;
        let Ok(entry) = entry else {
            continue;
        };
        if entry.file_type().is_some_and(|kind| kind.is_file()) {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}

/// 创建包含隐藏文件但遵循 Git 忽略规则且不跟随符号链接的遍历器。
fn walk_builder(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false);
    builder
}

/// 生成用于 Glob 过滤的稳定相对路径。
fn relative_for_filter(root: &Path, path: &Path, root_is_file: bool) -> String {
    if root_is_file {
        return path
            .file_name()
            .map(PathBuf::from)
            .as_deref()
            .map(display_path)
            .unwrap_or_else(|| display_path(path));
    }
    display_path(path.strip_prefix(root).unwrap_or(path))
}

/// 在同步搜索操作的安全点响应取消。
fn ensure_not_cancelled(cancellation: &TurnCancellation) -> Result<(), ToolError> {
    if cancellation.is_cancelled() {
        Err(ToolError::permanent("cancelled", "工具调用已取消"))
    } else {
        Ok(())
    }
}

/// 把 Tokio 阻塞任务异常归一为内部工具错误。
fn join_error(error: tokio::task::JoinError) -> ToolError {
    ToolError::permanent("blocking_task_failed", format!("搜索任务异常结束：{error}"))
}
