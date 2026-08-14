use std::path::{Path, PathBuf};

use async_trait::async_trait;
use peri_agent::{
    error::AgentResult,
    middleware::{r#trait::Middleware, state::MiddlewareState},
};

pub use peri_acp_types::agents::AgentOverrides;

use crate::agent_parser::{parse_project_agent, validate_agent_id};

/// AgentDefineMiddleware - 根据 agent_id 注入 KeenCode 项目 Agent 定义文件。
///
/// 项目定义只读取 `{cwd}/.keencode/agents/{agent_id}.md`。内置 Agent 与插件
/// Agent 由各自装配入口读取，不进入项目目录搜索。
///
/// Agent 定义文件格式（Claude Code YAML frontmatter）：
/// ```markdown
/// ---
/// name: code-reviewer
/// description: Reviews code for quality and best practices
/// tools: [Read, Glob, Grep]
/// tone: |
///   Be thorough and explain your reasoning in detail.
/// proactiveness: |
///   Proactively review related files and suggest improvements.
/// ---
///
/// You are a code reviewer. Focus on code quality and best practices.
/// ```
pub struct AgentDefineMiddleware;

impl AgentDefineMiddleware {
    /// 创建无状态的项目 Agent 定义中间件。
    pub fn new() -> Self {
        Self
    }

    /// 根据 cwd 和 agent_id 构建唯一项目定义路径。
    ///
    /// 非法 Agent ID 返回空列表，防止路径遍历和多套命名格式并存。
    pub fn candidate_paths(cwd: &str, agent_id: &str) -> Vec<PathBuf> {
        if validate_agent_id(agent_id).is_err() {
            return Vec::new();
        }
        vec![Path::new(cwd)
            .join(".keencode")
            .join("agents")
            .join(format!("{agent_id}.md"))]
    }

    /// 返回普通的项目 Agent 目录；缺失、符号链接或非目录都视为不可用。
    pub(crate) fn project_agents_dir(cwd: &str) -> Option<PathBuf> {
        let keencode_dir = Path::new(cwd).join(".keencode");
        let agents_dir = keencode_dir.join("agents");
        for directory in [&keencode_dir, &agents_dir] {
            let metadata = std::fs::symlink_metadata(directory).ok()?;
            if !metadata.file_type().is_dir() {
                tracing::warn!(
                    path = %directory.display(),
                    "KeenCode 项目 Agent 路径不是普通目录，已忽略"
                );
                return None;
            }
        }
        Some(agents_dir)
    }

    /// 解析唯一项目定义文件，并拒绝最终路径上的符号链接或非普通文件。
    ///
    /// `Ok(None)` 表示项目 Agent 目录或目标文件不存在；目标路径存在但类型
    /// 不合法时返回错误，使同 ID 的内置 Agent 不会被静默启用。
    pub(crate) fn project_agent_file(cwd: &str, agent_id: &str) -> Result<Option<PathBuf>, String> {
        validate_agent_id(agent_id)?;
        let Some(agents_dir) = Self::project_agents_dir(cwd) else {
            return Ok(None);
        };
        let expected_file_name = format!("{agent_id}.md");
        let entries = std::fs::read_dir(&agents_dir).map_err(|error| {
            format!(
                "无法读取项目 Agent 目录 '{}': {error}",
                agents_dir.display()
            )
        })?;
        let mut path = None;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "无法读取项目 Agent 目录项 '{}': {error}",
                    agents_dir.display()
                )
            })?;
            if entry.file_name() == std::ffi::OsStr::new(&expected_file_name) {
                path = Some(entry.path());
                break;
            }
        }
        let Some(path) = path else {
            return Ok(None);
        };
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "无法读取项目 Agent 定义元数据 '{}': {error}",
                    path.display()
                ));
            }
        };
        if !metadata.file_type().is_file() {
            return Err(format!(
                "项目 Agent 定义必须是普通文件，不能是符号链接或目录: '{}'",
                path.display()
            ));
        }
        Ok(Some(path))
    }

    /// 同步读取 agent.md，返回可覆盖 system prompt 的各个部分。
    ///
    /// 供 TUI 层在构建 LLM 前提前获取覆盖内容，传入 `build_system_prompt`。
    /// 返回 `None` 表示文件不存在或无有效内容。
    pub fn load_overrides(cwd: &str, agent_id: &str) -> Option<AgentOverrides> {
        let path = Self::project_agent_file(cwd, agent_id).ok()??;
        let content = std::fs::read_to_string(&path).ok()?;
        if content.trim().is_empty() {
            return None;
        }

        let agent = parse_project_agent(agent_id, &content).ok()?;
        let persona = if agent.system_prompt.is_empty() {
            None
        } else {
            Some(agent.system_prompt)
        };
        let overrides = AgentOverrides {
            persona,
            tone: agent.frontmatter.tone,
            proactiveness: agent.frontmatter.proactiveness,
            mode: agent.frontmatter.prompt_mode,
        };
        if overrides.is_empty() {
            None
        } else {
            Some(overrides)
        }
    }
}

impl Default for AgentDefineMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for AgentDefineMiddleware {
    fn name(&self) -> &str {
        "AgentDefineMiddleware"
    }

    async fn before_agent(&self, _state: &mut dyn MiddlewareState) -> AgentResult<()> {
        // 覆盖注入已在构建 LLM 时通过 build_system_prompt(overrides, cwd) 完成，
        // 中间件层无需再操作消息列表。
        //
        // [设计说明 /agent 覆盖功能停用]（crate-audit 2026-07-06 P2-5 调查结论 A）
        //
        // `/agent` 覆盖功能在**主 Agent 链路**显式停用（executor.rs 内
        // `agent_overrides: None` 硬编码）。这是有意设计而非 bug：
        //
        // 1. **主 Agent 无 `/agent` 身份**：主 Agent 不携带 persona/tone/proactiveness，
        //    仅 SubAgent 才有（来自 `.keencode/agents/{agent_id}.md` 的 frontmatter）。
        // 2. **SubAgent 覆盖走工具调用路径**：`SubAgentTool::invoke` →
        //    `overrides_from_agent_def`（`build_agent.rs:136`）从已解析的 agent 文件
        //    抽取 overrides，再通过 `system_builder`（`builder.rs:365`）传给
        //    `build_system_prompt(overrides, cwd)`。SubAgent **不需要**本中间件注入。
        // 3. **`load_overrides` 仅测试调用**：本中间件的 `load_overrides` 从磁盘读
        //    `.keencode/agents/{id}.md`，但生产 SubAgent 路径已通过工具调用解析获取
        //    overrides，无需中途重读盘（与 CLAUDE.md [TRAP]「SubAgent 必须复用 main
        //    agent frozen 的 CLAUDE.md/Skills，禁止重新读盘」一致）。保留 `load_overrides`
        //    作为 helper / 测试入口。
        //
        // 详见 `docs/design/peri-agent-system-prompt-v2.md` §125-136。
        Ok(())
    }
}

#[cfg(test)]
#[path = "agent_define_test.rs"]
mod tests;
