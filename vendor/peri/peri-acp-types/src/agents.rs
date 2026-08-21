//! agent 定义契约（agent.md 覆盖项 + 能力标签）。
//!
//! 自 `peri-middlewares`（`agent_define` / `scan_agents_detailed`）迁入
//! （3.0 批 2 波 1：协议类型归契约层；middlewares 保留 re-export 保兼容）。

/// agent.md 中可覆盖 system prompt 的部分
///
/// 所有字段均为 `Option`，`None` 表示使用默认值。
#[derive(Debug, Clone, Default)]
pub struct AgentOverrides {
    /// 角色定位（替换 `{{persona}}`）
    pub persona: Option<String>,
    /// 输出风格（替换 `{{tone_and_style}}`）
    pub tone: Option<String>,
    /// 主动性（替换 `{{proactiveness}}`）
    pub proactiveness: Option<String>,
    /// agent.md frontmatter 中 prompt_mode 的值："extend"|"full"，默认 extend。
    /// `full` 只替换 PersonaDomain 层（persona/domain instructions）；
    /// 安全与授权、工程行为、能力契约与运行时边界层始终保留，不会被移除。
    pub mode: Option<String>,
}

impl AgentOverrides {
    pub fn is_empty(&self) -> bool {
        self.persona.is_none() && self.tone.is_none() && self.proactiveness.is_none()
    }
}

/// 解析可选的 KeenCode provider/model 编码。
///
/// 返回 `Some((provider_id, model))` 表示输入使用 `provider_id::model`；返回
/// `None` 表示输入没有限定 provider。任何控制字符、空输入以及带空
/// provider/model 的限定编码都会被拒绝，避免不同运行入口各自宽松解析。
pub fn split_provider_model(value: &str) -> Result<Option<(&str, &str)>, &'static str> {
    if value.chars().any(char::is_control) {
        return Err("模型选择不能包含控制字符");
    }

    let value = value.trim();
    if value.is_empty() {
        return Err("模型选择不能为空");
    }

    let Some((provider_id, model)) = value.split_once("::") else {
        return Ok(None);
    };
    let provider_id = provider_id.trim();
    let model = model.trim();
    if provider_id.is_empty() || model.is_empty() {
        return Err("provider_id::model 的 provider_id 和 model 均不能为空");
    }

    Ok(Some((provider_id, model)))
}

/// 归一化 Agent 的显式模型选择。
///
/// Agent 模型只有一种显式编码：`provider_id::model`。省略 `model` 字段由
/// 调用方以 `None` 表示跟随当前会话；裸模型名不是有效输入。
pub fn normalize_agent_model(value: &str) -> Result<String, String> {
    let value = value.trim();
    let Some((provider_id, model)) =
        split_provider_model(value).map_err(|error| error.to_string())?
    else {
        return Err(format!(
            "不支持的 Agent 模型选择 '{value}'；应为 provider_id::model，省略 model 表示跟随当前会话"
        ));
    };
    Ok(format!("{provider_id}::{model}"))
}

/// agent 能力标签（subagent catalog 检索依据；由 agent.md 推断）。
///
/// `can_mutate` 是**保守调度提示**，不是代码级锁或安全边界：
/// 实际能力由 `filter_tools` 在工具注册层真裁剪，标签仅间接影响主模型
/// 的并行决策（见审计 prompt-sections-audit.md P1-8 修正后判定）。
#[derive(Debug, Clone)]
pub struct AgentCapability {
    /// 该 agent 是否会修改项目代码（保守推断，D5）。
    /// 只有能根据最终注册工具集合证明无项目写能力时才为 false：
    /// - omitted tools（继承父工具）含 Bash / folder_operations 等 → true，
    ///   除非显式 disallow 全部核心写能力工具；
    /// - 显式 `tools: []` → false（零工具）；
    /// - 白名单含任一写能力工具（Bash / Write / Edit / folder_operations /
    ///   cron_register / mcp__*）→ true。
    ///
    /// `allowedWriteDirs` 声明的 WriteSandbox 不计入 can_mutate，
    /// 因为沙箱目录不在项目代码范围内，agent 仍可并行调度。
    pub can_mutate: bool,
}

#[cfg(test)]
mod tests {
    use super::{normalize_agent_model, split_provider_model};

    /// provider/model 编码应在所有运行入口共享同一严格语法。
    #[test]
    fn provider_model_requires_non_empty_segments_without_control_characters() {
        assert_eq!(
            split_provider_model(" provider-a :: model-a ").unwrap(),
            Some(("provider-a", "model-a"))
        );
        for invalid in [
            "",
            "::model",
            "provider::",
            "provider::   ",
            "provider\n::model",
        ] {
            assert!(split_provider_model(invalid).is_err(), "{invalid:?}");
        }
    }

    /// Agent 只接受 KeenCode 限定模型；其他裸值均拒绝。
    #[test]
    fn agent_model_normalization_accepts_only_provider_qualified_models() {
        assert_eq!(
            normalize_agent_model("provider-a::model-a").unwrap(),
            "provider-a::model-a".to_string()
        );
        for invalid in ["", "unqualified-model"] {
            assert!(normalize_agent_model(invalid).is_err(), "{invalid}");
        }
    }
}
