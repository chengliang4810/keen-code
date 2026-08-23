//! 装配注入端口（3.0 批 2 波 2）。
//!
//! 资源类（`McpClientPool` / `CronScheduler` / `ToolSearchIndex` /
//! `CronScheduler`）与业务操作面（skills 扫描 / plugin 管理）在
//! peri-acp 协议面不再直接引用具体实现；宿主装配点构造具体实例后
//! upcast 为端口注入，ACP 侧只持端口接口（`docs/top-level.md` §0 依赖方向）。
//!
//! 具体实现位于 `peri-middlewares`（端口 impl 归实现方）。
//! `downcast_arc` 为还原点：middlewares 装配面（`assembly.rs:127-152`）与
//! 装配面宿主经 `as_any` 还原具体类型调用业务方法（与 `TaskManager`
//! downcast 先例一致）。

use std::any::{Any, TypeId};
use std::path::PathBuf;
use std::sync::Arc;

use crate::agents::AgentCapability;
use crate::skills::{SkillMetadata, SkillRoot};

/// MCP 客户端池端口（`peri-middlewares::mcp::McpClientPool` 实现）。
///
/// 宿主装配点构造 `McpClientPool` 后 upcast 注入；ACP 协议面只传递
/// 句柄，工具桥接/服务器管理在装配面宿主（`host/stage_builder.rs` /
/// `host/stage_builder.rs`）。`shutdown` / `snapshot` 为 M-TUI 收口新增
/// 数据端口（`host/shutdown` 与 `mcp/list` 命令面经此访问，TUI 不再
/// 直持具体句柄）。
#[async_trait::async_trait]
pub trait McpPoolPort: Send + Sync {
    /// 还原具体实现（downcast 还原点，供 middlewares 装配面与装配面宿主使用）。
    fn as_any(&self) -> &dyn Any;

    /// 关闭连接池（`host/shutdown` 命令面调用；与 `McpClientPool::shutdown`
    /// 语义一致）。
    async fn shutdown(&self);

    /// 池状态快照（`mcp/list` 命令面数据源）：`{"initPhase": ..., "servers":
    /// [...]}`，字段语义与 TUI 面板投影一致（序列化格式由实现方保证，
    /// 契约层不透传具体类型）。
    fn snapshot(&self) -> serde_json::Value;
}

impl dyn McpPoolPort {
    /// 将 `Arc<dyn McpPoolPort>` 还原为具体实现 `Arc<T>`（类型不符返回原 `Arc`）。
    pub fn downcast_arc<T: McpPoolPort + 'static>(self: Arc<Self>) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            // 经 `as_any()` 取具体类型的 TypeId：直接对 trait object 调
            // `type_id()` 会命中 `Any` 的 blanket impl，返回
            // `TypeId::of::<dyn McpPoolPort>()`（trait object 自身），
            // 恒不等于 `TypeId::of::<T>()` → downcast 恒失败 → 装配面回退
            // 临时实例，注入的连接池与装配产物分离（同构
            // 失败时保持端口对象不变，避免装配出的连接池与宿主分离。
            if (*ptr).as_any().type_id() == TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}

/// 工具检索索引端口（`peri-middlewares::tool_search::ToolSearchIndex` 实现）。
pub trait ToolSearchPort: Send + Sync {
    /// 还原具体实现（downcast 还原点，供 middlewares 装配面与装配面宿主使用）。
    fn as_any(&self) -> &dyn Any;

    /// 清空当前索引。
    ///
    /// Host 在一个 session 的新 turn 边界调用此方法；它不是 Host 级
    /// registry 的清理机制。默认实现保持外部端口实现兼容，生产索引覆盖
    /// 为真正的清理操作。
    fn clear(&self) {}
}

impl dyn ToolSearchPort {
    /// 将 `Arc<dyn ToolSearchPort>` 还原为具体实现 `Arc<T>`（类型不符返回原 `Arc`）。
    pub fn downcast_arc<T: ToolSearchPort + 'static>(self: Arc<Self>) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            // 经 `as_any()` 取具体类型的 TypeId：直接对 trait object 调
            // `type_id()` 会命中 `Any` 的 blanket impl，返回
            // `TypeId::of::<dyn ToolSearchPort>()`（trait object 自身），
            // 恒不等于 `TypeId::of::<T>()` → downcast 恒失败 → 装配面回退
            // 默认实例，注入的搜索索引与装配产物分离（同构
            // 失败时保持端口对象不变，避免装配出的索引与宿主分离。
            if (*ptr).as_any().type_id() == TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}

/// LSP 服务器池端口（`peri-lsp::pool::LspServerPool` 实现）。
///
/// per-session 实例；构造点（装配面宿主 `host/requests.rs` 经
/// `peri_middlewares::assembly::create_session_lsp_pool`
/// 创建）持有具体实现，协议面只持端口句柄。装配面（`assembly.rs`
/// `ChainSlot::Lsp`）经 `downcast_arc` 还原具体类型复用同一 pool——
/// 服务器进程、initialized 状态与诊断注册表跨 turn 存活（H1：
/// 每 turn 重建 pool 导致冷启动与状态丢失）。宿主退出（`run_acp_server` 返回）
/// 经 `shutdown` 优雅关闭全部服务器子进程。
#[async_trait::async_trait]
pub trait LspPoolPort: Send + Sync {
    /// 还原具体实现（downcast 还原点，供 middlewares 装配面使用）。
    fn as_any(&self) -> &dyn Any;

    /// 优雅关闭全部服务器（发送 shutdown/exit 并终止子进程；幂等）。
    async fn shutdown(&self);
}

impl dyn LspPoolPort {
    /// 将 `Arc<dyn LspPoolPort>` 还原为具体实现 `Arc<T>`（类型不符返回原 `Arc`）。
    pub fn downcast_arc<T: LspPoolPort + 'static>(self: Arc<Self>) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            // 经 `as_any()` 取具体类型的 TypeId：直接对 trait object 调
            // `type_id()` 会命中 `Any` 的 blanket impl，返回
            // `TypeId::of::<dyn LspPoolPort>()`（trait object 自身），
            // 恒不等于 `TypeId::of::<T>()` → downcast 恒失败 → 装配面回退
            // 临时实例，会话级 pool 与装配产物分离（同构
            // 失败时保持端口对象不变，避免会话级服务器池与宿主分离。
            if (*ptr).as_any().type_id() == TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}

/// Skills 扫描端口：协议命令面（available-commands / skill 列表 / agent 列表）
/// 经此访问 skills/agents 扫描业务，具体扫描逻辑留在 `peri-middlewares`
/// （`SkillsMiddleware::resolve_roots_static` / `scan_skill_roots` /
/// `scan_agents_detailed`）。
pub trait SkillsPort: Send + Sync {
    /// 解析 skill 根目录并扫描全部 skill 元数据（含 bundled 禁用判定）。
    fn available_skills(&self, cwd: &str, plugin_roots: &[SkillRoot]) -> Vec<SkillMetadata>;

    /// 扫描可调度 agent 目录，返回 `(agent_id, name, description, capability)`。
    fn agents(
        &self,
        cwd: &str,
        extra_dirs: &[PathBuf],
    ) -> Vec<(String, String, String, AgentCapability)>;
}

#[cfg(test)]
#[path = "ports_test.rs"]
mod tests;
