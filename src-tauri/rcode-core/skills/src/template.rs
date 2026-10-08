//! Skill 与 command 共用的单遍参数展开，不执行 Shell、不递归展开用户参数。

/// 展开 Claude Code 的 `$ARGUMENTS`、`$ARGUMENTS[N]` 和从零开始的 `$N`。
/// 参数支持引号与转义；无占位符时追加 `ARGUMENTS:`。无效输入或超限返回 `None`。
pub fn render_skill_arguments(markdown: &str, arguments: &str, maximum: usize) -> Option<String> {
    if arguments.contains('\0') || arguments.len() > maximum || markdown.len() > maximum {
        return None;
    }
    let words = shlex::split(arguments)?;
    let mut output = String::new();
    let mut remaining = markdown;
    let mut substituted = false;
    while let Some(index) = remaining.find('$') {
        let prefix = &remaining[..index];
        if output.len().checked_add(prefix.len())? > maximum {
            return None;
        }
        output.push_str(prefix);
        remaining = &remaining[index + 1..];
        let positional = remaining
            .strip_prefix("ARGUMENTS[")
            .and_then(|value| {
                let end = value.find(']')?;
                let position = value[..end].parse::<usize>().ok()?;
                Some((position, "ARGUMENTS[".len() + end + 1))
            })
            .or_else(|| {
                let count = remaining.bytes().take_while(u8::is_ascii_digit).count();
                (count > 0)
                    .then(|| {
                        remaining[..count]
                            .parse::<usize>()
                            .ok()
                            .map(|position| (position, count))
                    })
                    .flatten()
            });
        let token = if let Some((position, count)) = positional {
            Some((words.get(position).map(String::as_str).unwrap_or(""), count))
        } else if remaining.strip_prefix("ARGUMENTS").is_some_and(|tail| {
            tail.chars()
                .next()
                .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_')
        }) {
            Some((arguments, "ARGUMENTS".len()))
        } else {
            None
        };
        if let Some((value, consumed)) = token {
            if output.len().checked_add(value.len())? > maximum {
                return None;
            }
            output.push_str(value);
            remaining = &remaining[consumed..];
            substituted = true;
        } else {
            if output.len() == maximum {
                return None;
            }
            output.push('$');
        }
    }
    if output.len().checked_add(remaining.len())? > maximum {
        return None;
    }
    output.push_str(remaining);
    if !substituted && !arguments.trim().is_empty() {
        let suffix = format!("\n\nARGUMENTS: {arguments}");
        if output.len().checked_add(suffix.len())? > maximum {
            return None;
        }
        output.push_str(&suffix);
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expands_once_with_quoted_zero_based_arguments_and_bounded_output() {
        assert_eq!(
            render_skill_arguments("$0 / $ARGUMENTS[1] / $ARGUMENTS", "'one two' '$0'", 100)
                .unwrap(),
            "one two / $0 / 'one two' '$0'"
        );
        assert_eq!(
            render_skill_arguments("body", "hello", 100).unwrap(),
            "body\n\nARGUMENTS: hello"
        );
        assert_eq!(
            render_skill_arguments("$ARGUMENTS_SUFFIX", "", 100).unwrap(),
            "$ARGUMENTS_SUFFIX"
        );
        assert!(render_skill_arguments("$0$0$0", "1234", 10).is_none());
        assert!(render_skill_arguments("$0", "'unclosed", 100).is_none());
    }
}
