//! 把安全发现的 Skills 目录暴露为按需加载工具。

use std::sync::Arc;

use keencode_agent::{
    AgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
};
use keencode_model::ToolDefinition;
use keencode_skills::{SkillCatalog, SkillLoadError, SkillSource};
use serde::Deserialize;
use serde_json::{Value, json};

/// 工具输入允许的 Skill 名称最大字节数。
const MAX_SKILL_NAME_BYTES: usize = 257;

/// 从冻结目录中安全读取单个 `SKILL.md` 正文的工具。
pub struct SkillTool {
    /// 已完成来源优先级、禁用状态和路径安全归约的目录。
    catalog: Arc<SkillCatalog>,
}

impl SkillTool {
    /// 创建绑定到指定 Skills 目录快照的按需加载工具。
    pub fn new(catalog: Arc<SkillCatalog>) -> Self {
        Self { catalog }
    }
}

impl AgentTool for SkillTool {
    /// 返回只接受一个稳定 Skill 名称的严格输入 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Skill",
            "Load a discovered and enabled skill by name. Call only when its catalog description matches the current task. Returned Markdown provides task guidance and does not automatically execute commands.",
            json!({
                "type": "object",
                "properties": {
                    "arguments": { "type": "string", "maxLength": 65536, "description": "Arguments to substitute in the skill template" },
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_SKILL_NAME_BYTES,
                        "description": "Exact name shown in the skills catalog"
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        )
    }

    /// 加载 Skill 只读取本地文档，不修改文件或其他外部状态。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ReadOnly)
    }

    /// 不同 Skill 的有界读取可以安全并行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 校验名称，在阻塞线程复核路径并读取正文，最后返回稳定 JSON。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let catalog = self.catalog.clone();
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            let input: SkillInput = serde_json::from_value(input).map_err(|error| {
                ToolError::permanent("invalid_input", format!("Skill 输入无效：{error}"))
            })?;
            validate_skill_name(&input.name)?;

            let name = input.name;
            let mut loaded = tokio::task::spawn_blocking(move || catalog.load(&name))
                .await
                .map_err(|_| ToolError::retryable("skill_worker_failed", "Skill 读取线程意外终止"))?
                .map_err(map_load_error)?;
            if context.cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            if loaded.disable_model_invocation {
                return Err(ToolError::permanent(
                    "skill_model_invocation_disabled",
                    "该 Skill 仅允许用户显式调用",
                ));
            }

            loaded.markdown = loaded
                .markdown
                .replace("${CLAUDE_SESSION_ID}", context.session_id.as_str());
            loaded.markdown = keencode_skills::render_skill_arguments(
                &loaded.markdown,
                input.arguments.as_deref().unwrap_or(""),
                320 * 1024,
            )
            .ok_or_else(|| {
                ToolError::permanent("skill_arguments_invalid", "Skill 参数无效或展开结果超限")
            })?;
            let source = match loaded.source {
                SkillSource::Project => "project",
                SkillSource::Data => "data",
                SkillSource::Plugin => "plugin",
            };
            let text = serde_json::to_string(&json!({
                "name": loaded.name,
                "description": loaded.description,
                "source": source,
                "directory": loaded.directory,
                "markdown": loaded.markdown
            }))
            .map_err(|error| {
                ToolError::permanent(
                    "skill_output_failed",
                    format!("Skill 结果无法序列化：{error}"),
                )
            })?;
            Ok(ToolOutput::text(text))
        })
    }
}

/// Skill 工具的严格顶层输入。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillInput {
    /// 目录中已发现 Skill 的精确或 ASCII 大小写不敏感名称。
    name: String,
    /// 用户传递的模板参数，不作为 shell 执行。
    #[serde(default)]
    arguments: Option<String>,
}

/// 在目录查找前限制名称体积并拒绝路径式或不稳定标识。
fn validate_skill_name(name: &str) -> Result<(), ToolError> {
    let bytes = name.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= MAX_SKILL_NAME_BYTES
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && !name.contains("..")
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
    if !valid {
        return Err(ToolError::permanent(
            "invalid_input",
            "Skill 名称必须是有界的稳定非路径 ASCII 标识",
        ));
    }
    Ok(())
}

/// 把 Skills 核心的安全错误映射为稳定工具错误码。
fn map_load_error(error: SkillLoadError) -> ToolError {
    match error {
        SkillLoadError::NotFound { .. } => {
            ToolError::permanent("skill_not_found", error.to_string())
        }
        SkillLoadError::Disabled { .. } => {
            ToolError::permanent("skill_disabled", error.to_string())
        }
        SkillLoadError::RootChanged { .. } => {
            ToolError::retryable("skill_catalog_stale", error.to_string())
        }
        SkillLoadError::CatalogStale { .. } => {
            ToolError::retryable("skill_catalog_stale", error.to_string())
        }
        SkillLoadError::Unavailable { .. } => {
            ToolError::retryable("skill_unavailable", error.to_string())
        }
        SkillLoadError::ReadFailed { .. } => {
            ToolError::retryable("skill_read_failed", error.to_string())
        }
        SkillLoadError::UnsafePath { .. } => {
            ToolError::permanent("skill_unsafe_path", error.to_string())
        }
        SkillLoadError::TooLarge { .. } => {
            ToolError::permanent("skill_too_large", error.to_string())
        }
        SkillLoadError::InvalidDocument { .. } => {
            ToolError::permanent("skill_invalid_document", error.to_string())
        }
    }
}

/// 返回 Turn 取消时统一使用的不可重试工具错误。
fn cancelled_error() -> ToolError {
    ToolError::permanent("skill_cancelled", "Skill 加载因当前 Turn 取消而停止")
}
