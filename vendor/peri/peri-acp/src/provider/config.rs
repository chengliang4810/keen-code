//! 配置类型定义 — 与 ~/.peri/settings.json 对应
//!
//! 从 peri-tui 迁移，移除 TUI 特有关联。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 顶层包装（与 ~/.peri/settings.json 的 { "config": {...} } 对应）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeriConfig {
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default)]
    pub config: AppConfig,
}

/// Provider 内的模型元数据。
///
/// 运行时只接受调用方明确提供的 `provider_id::model`；该 map 仅保留配置中
/// 可能存在的逐模型元数据，不参与子 Agent 选择。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderModels {
    #[serde(flatten)]
    pub models: HashMap<String, Value>,
}

/// Beta 功能开关配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BetasConfig {}

/// 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// 全局 skills 目录路径
    #[serde(default, alias = "skillsDir")]
    pub skills_dir: Option<String>,
    /// 环境变量注入
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// Compact 系统配置
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact: Option<peri_acp_types::compact::CompactConfig>,
    /// UI 语言
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// 系统提示词 persona 覆盖
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// 系统提示词 tone 覆盖
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    /// CLAUDE.md 排除 glob 模式列表
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_md_excludes: Option<Vec<String>>,
    /// 主动性级别
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proactiveness: Option<String>,
    /// 是否在消息流中显示缓存命中率过低警告。
    /// Option<bool>：None=未设置（merge 时保留全局值），Some=显式开/关。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_cache_warning: Option<bool>,
    /// Beta 功能开关
    #[serde(default)]
    pub betas: BetasConfig,
}

impl AppConfig {
    /// 用 workspace 配置覆盖全局配置。
    /// workspace 中出现的字段替换全局对应字段，未出现的保留全局值。
    pub fn merge_overrides(&mut self, workspace: AppConfig) {
        // providers — 空列表视为"未填写"，不覆盖
        if !workspace.providers.is_empty() {
            self.providers = workspace.providers;
        }
        // Option<T> 字段 — is_some() 则覆盖
        if workspace.skills_dir.is_some() {
            self.skills_dir = workspace.skills_dir;
        }
        if workspace.env.is_some() {
            self.env = workspace.env;
        }
        if workspace.compact.is_some() {
            self.compact = workspace.compact;
        }
        if workspace.language.is_some() {
            self.language = workspace.language;
        }
        if workspace.persona.is_some() {
            self.persona = workspace.persona;
        }
        if workspace.tone.is_some() {
            self.tone = workspace.tone;
        }
        if workspace.claude_md_excludes.is_some() {
            self.claude_md_excludes = workspace.claude_md_excludes;
        }
        if workspace.proactiveness.is_some() {
            self.proactiveness = workspace.proactiveness;
        }
        // show_cache_warning: 仅当 workspace 显式设置时才覆盖（避免默认 false 冲掉全局开启）
        if workspace.show_cache_warning.is_some() {
            self.show_cache_warning = workspace.show_cache_warning;
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            skills_dir: None,
            env: None,
            compact: None,
            language: None,
            persona: None,
            tone: None,
            proactiveness: None,
            claude_md_excludes: None,
            show_cache_warning: None,
            betas: BetasConfig::default(),
        }
    }
}

/// 单个 Provider 配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    #[serde(default)]
    pub id: String,
    /// "openai" | "anthropic" 等
    #[serde(rename = "type", default)]
    pub provider_type: String,
    #[serde(rename = "apiKey", default)]
    pub api_key: String,
    /// OpenAI Base URL
    #[serde(rename = "baseUrl", default)]
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub models: ProviderModels,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ProviderConfig {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
