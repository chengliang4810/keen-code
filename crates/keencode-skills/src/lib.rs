//! KeenCode Skills 的 Provider 中立领域与安全加载模块。
//!
//! 本 crate 只发现、解析和按需读取 `SKILL.md`，不会执行 Skill 中声明的脚本、
//! 命令或其他资源。目录阶段只返回名称、说明、来源和启用状态；正文只在调用方
//! 明确按名称加载时读取。

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod loader;
mod model;
mod parser;

pub use loader::{SkillCatalog, discover_skills};
pub use model::{
    InjectableSkill, ParsedSkillDocument, SkillCatalogEntry, SkillConfigError, SkillDiagnostic,
    SkillDiagnosticCode, SkillDiagnosticSeverity, SkillDirectories, SkillDiscoveryConfig,
    SkillLimits, SkillLoadError, SkillRoot, SkillSource,
};
pub use parser::{SkillDocumentError, parse_skill_document};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
