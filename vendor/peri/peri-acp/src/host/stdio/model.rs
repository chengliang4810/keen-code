//! 模型切换辅助，供 set_model / set_config_option 的 model 分支复用。

use super::context::StdioContext;

/// 切换模型并失效 Agent 缓存。
///
/// 返回切换后的模型名（None 表示 model_id 无效，切换失败）。
pub(super) fn switch_model(ctx: &StdioContext, sid: &str, model_id: &str) -> Option<String> {
    let new_provider = {
        let cfg = ctx.peri_config.read();
        let (provider_id, model) = model_id.split_once("::")?;
        crate::provider::LlmProvider::from_provider_config(
            &cfg,
            provider_id,
            model,
            ctx.provider.read().effort().map(str::to_owned),
            32_000,
            ctx.provider.read().context_1m(),
            None,
        )
    };
    let name = new_provider.as_ref().map(|p| p.model_name().to_string());
    if let Some(p) = new_provider {
        tracing::info!(model_id = %model_id, model = %p.model_name(), "Model changed");
        *ctx.provider.write() = p;
    }
    let mut sessions = ctx.sessions.write();
    if let Some(s) = sessions.get_mut(sid) {
        s.agent_pool.invalidate();
    }
    name
}
