//! Agent construction and lifecycle.
//!
//! 3.0 批 2 + L5 归位：`build_agent` / `build_stage_context`（AgentComponents 装配）
//! 装配桥在 `crate::host::stage_builder`（装配注入面，装配本体在
//! peri-agent session 工厂）。
