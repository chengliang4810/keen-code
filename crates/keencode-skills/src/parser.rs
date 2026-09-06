//! `SKILL.md` 的有界 front matter 与 Markdown 正文解析。

use crate::{ParsedSkillDocument, SkillLimits};
use std::error::Error;
use std::fmt;

/// `SKILL.md` 不符合安全子集时返回的解析错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SkillDocumentError {
    /// 文档没有以独立的 `---` 行开始。
    MissingFrontMatter,
    /// Front matter 在配置上限内没有闭合。
    UnclosedFrontMatter,
    /// `name` 或 `description` 字段缺失。
    MissingField {
        /// 缺失字段的稳定名称。
        field: &'static str,
    },
    /// `name` 或 `description` 被重复声明。
    DuplicateField {
        /// 重复字段的稳定名称。
        field: &'static str,
    },
    /// 目标字段使用了不支持或无效的标量形式。
    InvalidField {
        /// 无效字段的稳定名称。
        field: &'static str,
        /// 不包含原始字段值的失败原因。
        reason: &'static str,
    },
    /// Skill 名称不符合稳定、不可作为路径的标识规则。
    InvalidName,
    /// 字段超过配置的 UTF-8 字节上限。
    FieldTooLarge {
        /// 超限字段的稳定名称。
        field: &'static str,
        /// 允许的最大 UTF-8 字节数。
        limit: usize,
    },
}

impl fmt::Display for SkillDocumentError {
    /// 输出不回显不可信 front matter 内容的解析错误。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFrontMatter => formatter.write_str("缺少 front matter 起始分隔符"),
            Self::UnclosedFrontMatter => formatter.write_str("front matter 未在上限内闭合"),
            Self::MissingField { field } => write!(formatter, "front matter 缺少 {field}"),
            Self::DuplicateField { field } => write!(formatter, "front matter 重复声明 {field}"),
            Self::InvalidField { field, reason } => {
                write!(formatter, "front matter 字段 {field} 无效：{reason}")
            }
            Self::InvalidName => formatter.write_str("Skill name 必须是稳定的非路径标识"),
            Self::FieldTooLarge { field, limit } => {
                write!(formatter, "front matter 字段 {field} 超过 {limit} 字节")
            }
        }
    }
}

impl Error for SkillDocumentError {}

/// 解析 UTF-8 `SKILL.md` 的名称、说明和 Markdown 正文。
///
/// 解析器只解释 `name` 与 `description`，允许其他顶层键但不会返回它们。
/// 两个目标字段支持普通标量、单引号、双引号以及 `|`、`>` 块标量。
pub fn parse_skill_document(
    content: &str,
    limits: &SkillLimits,
) -> Result<ParsedSkillDocument, SkillDocumentError> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = LinesWithOffsets::new(content);
    let first = lines.next().ok_or(SkillDocumentError::MissingFrontMatter)?;
    if first.text.trim_end_matches('\r') != "---" {
        return Err(SkillDocumentError::MissingFrontMatter);
    }

    let mut metadata_lines = Vec::new();
    let body_offset = loop {
        let line = lines
            .next()
            .ok_or(SkillDocumentError::UnclosedFrontMatter)?;
        if line.start > limits.max_front_matter_bytes {
            return Err(SkillDocumentError::UnclosedFrontMatter);
        }
        let text = line.text.trim_end_matches('\r');
        if text == "---" {
            if line.end > limits.max_front_matter_bytes {
                return Err(SkillDocumentError::UnclosedFrontMatter);
            }
            break line.end;
        }
        metadata_lines.push(text);
    };

    let mut name = None;
    let mut description = None;
    let mut index = 0;
    while index < metadata_lines.len() {
        let line = metadata_lines[index];
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            index += 1;
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            index += 1;
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once(':') else {
            index += 1;
            continue;
        };
        let key = raw_key.trim();
        if key != "name" && key != "description" {
            index += 1;
            continue;
        }
        let (value, consumed) = parse_scalar(key, raw_value, &metadata_lines[index + 1..])?;
        let slot = if key == "name" {
            &mut name
        } else {
            &mut description
        };
        if slot.replace(value).is_some() {
            return Err(SkillDocumentError::DuplicateField {
                field: field_name(key),
            });
        }
        index += consumed + 1;
    }

    let name = name.ok_or(SkillDocumentError::MissingField { field: "name" })?;
    let description = description.ok_or(SkillDocumentError::MissingField {
        field: "description",
    })?;
    if name.len() > limits.max_name_bytes {
        return Err(SkillDocumentError::FieldTooLarge {
            field: "name",
            limit: limits.max_name_bytes,
        });
    }
    if description.len() > limits.max_description_bytes {
        return Err(SkillDocumentError::FieldTooLarge {
            field: "description",
            limit: limits.max_description_bytes,
        });
    }
    if !is_valid_skill_name(&name) {
        return Err(SkillDocumentError::InvalidName);
    }

    Ok(ParsedSkillDocument {
        name,
        description,
        markdown: content.get(body_offset..).unwrap_or_default().to_string(),
    })
}

/// 把有效 Skill 名称转换为跨平台冲突与查找键。
pub(crate) fn normalized_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// 判断名称是否为不可解释成文件路径的稳定 ASCII 标识。
fn is_valid_skill_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || name.contains("..")
    {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// 将动态键映射为可进入错误枚举的静态字段名。
const fn field_name(key: &str) -> &'static str {
    if key.len() == 4 {
        "name"
    } else {
        "description"
    }
}

/// 解析单行或块标量并返回额外消耗的行数。
fn parse_scalar(
    key: &str,
    raw_value: &str,
    following: &[&str],
) -> Result<(String, usize), SkillDocumentError> {
    let value = raw_value.trim();
    if value.starts_with('|') || value.starts_with('>') {
        return parse_block_scalar(key, strip_plain_comment(value).trim_end(), following);
    }
    if value.is_empty() {
        return Err(SkillDocumentError::InvalidField {
            field: field_name(key),
            reason: "标量不能为空",
        });
    }
    if value.starts_with('\'') {
        return parse_single_quoted(key, value).map(|parsed| (parsed, 0));
    }
    if value.starts_with('"') {
        return parse_double_quoted(key, value).map(|parsed| (parsed, 0));
    }
    let plain = strip_plain_comment(value).trim_end();
    if plain.is_empty() {
        return Err(SkillDocumentError::InvalidField {
            field: field_name(key),
            reason: "标量不能为空",
        });
    }
    Ok((plain.to_string(), 0))
}

/// 解析 YAML 单引号标量，两个连续单引号表示一个单引号。
fn parse_single_quoted(key: &str, value: &str) -> Result<String, SkillDocumentError> {
    let mut parsed = String::new();
    let mut characters = value[1..].char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        if character != '\'' {
            parsed.push(character);
            continue;
        }
        if characters.peek().is_some_and(|(_, next)| *next == '\'') {
            parsed.push('\'');
            characters.next();
            continue;
        }
        let remainder = value[index + 2..].trim();
        if remainder.is_empty() || remainder.starts_with('#') {
            return Ok(parsed);
        }
        return Err(SkillDocumentError::InvalidField {
            field: field_name(key),
            reason: "单引号标量后包含无效内容",
        });
    }
    Err(SkillDocumentError::InvalidField {
        field: field_name(key),
        reason: "单引号标量未闭合",
    })
}

/// 解析只接受常用安全转义的 YAML 双引号标量。
fn parse_double_quoted(key: &str, value: &str) -> Result<String, SkillDocumentError> {
    let mut parsed = String::new();
    let mut chars = value[1..].char_indices();
    while let Some((index, character)) = chars.next() {
        if character == '"' {
            let remainder = value[index + 2..].trim();
            if remainder.is_empty() || remainder.starts_with('#') {
                return Ok(parsed);
            }
            return Err(SkillDocumentError::InvalidField {
                field: field_name(key),
                reason: "双引号标量后包含无效内容",
            });
        }
        if character != '\\' {
            parsed.push(character);
            continue;
        }
        let (_, escaped) = chars.next().ok_or(SkillDocumentError::InvalidField {
            field: field_name(key),
            reason: "双引号标量转义未完成",
        })?;
        match escaped {
            '\\' => parsed.push('\\'),
            '"' => parsed.push('"'),
            'n' => parsed.push('\n'),
            'r' => parsed.push('\r'),
            't' => parsed.push('\t'),
            _ => {
                return Err(SkillDocumentError::InvalidField {
                    field: field_name(key),
                    reason: "双引号标量包含不支持的转义",
                });
            }
        }
    }
    Err(SkillDocumentError::InvalidField {
        field: field_name(key),
        reason: "双引号标量未闭合",
    })
}

/// 解析 `|` 与 `>` 块标量的常用形式。
fn parse_block_scalar(
    key: &str,
    indicator: &str,
    following: &[&str],
) -> Result<(String, usize), SkillDocumentError> {
    let Some(style) = indicator.chars().next() else {
        unreachable!("调用方已经确认块标量指示符非空")
    };
    if !matches!(indicator, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
        return Err(SkillDocumentError::InvalidField {
            field: field_name(key),
            reason: "块标量指示符不受支持",
        });
    }
    let mut consumed = 0;
    let mut block = Vec::new();
    let mut minimum_indent = usize::MAX;
    for line in following {
        if line.trim().is_empty() {
            block.push(*line);
            consumed += 1;
            continue;
        }
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        if indent == 0 {
            break;
        }
        minimum_indent = minimum_indent.min(indent);
        block.push(*line);
        consumed += 1;
    }
    if block.is_empty() || minimum_indent == usize::MAX {
        return Err(SkillDocumentError::InvalidField {
            field: field_name(key),
            reason: "块标量没有缩进正文",
        });
    }
    let normalized: Vec<&str> = block
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                ""
            } else {
                line.get(minimum_indent..).unwrap_or_default()
            }
        })
        .collect();
    let mut value = if style == '|' {
        normalized.join("\n")
    } else {
        fold_block_lines(&normalized)
    };
    match indicator.chars().nth(1) {
        Some('-') => {}
        Some('+') => value.push('\n'),
        None => value.push('\n'),
        _ => unreachable!("块标量指示符已在前面校验"),
    }
    Ok((value, consumed))
}

/// 按 YAML 折叠标量的核心语义合并普通行并保留空行。
fn fold_block_lines(lines: &[&str]) -> String {
    let mut result = String::new();
    for (index, line) in lines.iter().enumerate() {
        result.push_str(line);
        let Some(next) = lines.get(index + 1) else {
            continue;
        };
        if line.is_empty() || next.is_empty() {
            result.push('\n');
        } else {
            result.push(' ');
        }
    }
    result
}

/// 去除普通标量中由空白分隔的 YAML 行尾注释。
fn strip_plain_comment(value: &str) -> &str {
    for (index, character) in value.char_indices() {
        if character == '#'
            && index > 0
            && value[..index]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
        {
            return &value[..index];
        }
    }
    value
}

/// 保留每行在原始 UTF-8 文本中的结束偏移。
struct LinesWithOffsets<'a> {
    /// 尚未返回的原始文本。
    remaining: &'a str,
    /// `remaining` 在原始文本中的起始字节偏移。
    offset: usize,
}

impl<'a> LinesWithOffsets<'a> {
    /// 从完整文档创建逐行迭代器。
    const fn new(content: &'a str) -> Self {
        Self {
            remaining: content,
            offset: 0,
        }
    }
}

/// 一行文本及其原始字节范围。
struct OffsetLine<'a> {
    /// 不包含换行符的行文本。
    text: &'a str,
    /// 行在原始文本中的起始字节偏移。
    start: usize,
    /// 下一行在原始文本中的起始字节偏移。
    end: usize,
}

impl<'a> Iterator for LinesWithOffsets<'a> {
    type Item = OffsetLine<'a>;

    /// 返回下一行，并使结束偏移包含原始换行符。
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }
        let start = self.offset;
        if let Some(index) = self.remaining.find('\n') {
            let text = &self.remaining[..index];
            let consumed = index + 1;
            self.remaining = &self.remaining[consumed..];
            self.offset += consumed;
            Some(OffsetLine {
                text,
                start,
                end: self.offset,
            })
        } else {
            let text = self.remaining;
            self.remaining = "";
            self.offset += text.len();
            Some(OffsetLine {
                text,
                start,
                end: self.offset,
            })
        }
    }
}
