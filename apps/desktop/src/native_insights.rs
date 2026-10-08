//! 原生设置页按需读取的用量和脱敏诊断；不会启动空闲轮询或额外采样线程。

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    agent_runtime::AgentRuntime,
    analytics::{
        self, AnalyticsRecorder, RequestRecordsPage, RequestRecordsQuery, TaskCacheUsage,
        UsageStats,
    },
    diagnostics::{
        Diagnostics,
        observability::{ObservabilitySnapshot, ResourceSample, now_epoch_ms},
        process_resources::ProcessResourceSample,
    },
    native_paths::NativePaths,
    plugins::{self, PluginManager},
};

/// 插件清单已声明该事件，但当前 Native 宿主没有对应的执行入口。
pub const UNSUPPORTED_PLUGIN_HOOK_REASON: &str = "当前 Native 执行入口未实现";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub stats: UsageStats,
    pub records: RequestRecordsPage,
    /// 当前任务只统计仍保留的请求记录；缺失 Provider 缓存字段时保持未知。
    pub task_cache: Option<TaskCacheUsage>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginHookDiagnostic {
    /// 已安装插件的稳定公开 ID，不包含安装路径或配置正文。
    pub plugin_id: String,
    /// 插件清单声明的 Hook 事件名。
    pub event: String,
    /// 对用户解释该事件不会运行的固定原因。
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiagnosticsSnapshot {
    pub log_path: std::path::PathBuf,
    pub resources: ProcessResourceSample,
    pub observability: ObservabilitySnapshot,
    /// 静态插件清单诊断；不把路径、凭据或配置正文带入设置投影。
    #[serde(default)]
    pub plugin_hook_diagnostics: Vec<PluginHookDiagnostic>,
}

fn plugin_hook_diagnostics_from_events(
    plugin_id: impl Into<String>,
    events: impl IntoIterator<Item = String>,
) -> Vec<PluginHookDiagnostic> {
    let plugin_id = plugin_id.into();
    events
        .into_iter()
        .map(|event| PluginHookDiagnostic {
            plugin_id: plugin_id.clone(),
            event,
            reason: UNSUPPORTED_PLUGIN_HOOK_REASON.to_owned(),
        })
        .collect()
}

fn collect_plugin_hook_diagnostics(data_root: &Path) -> Vec<PluginHookDiagnostic> {
    let manager = PluginManager::new(data_root.to_owned());
    let Ok(state) = manager.load_state() else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    for plugin in state.plugins {
        let Ok(manifest) = plugins::load_plugin_manifest(&plugin.install_path) else {
            continue;
        };
        let Ok(inventory) = plugins::inspect_plugin_components(&plugin.install_path, &manifest)
        else {
            continue;
        };
        diagnostics.extend(plugin_hook_diagnostics_from_events(
            plugin.id.to_string(),
            inventory.unsupported_hooks,
        ));
    }
    diagnostics.sort_by(|left, right| {
        left.plugin_id
            .cmp(&right.plugin_id)
            .then_with(|| left.event.cmp(&right.event))
    });
    diagnostics
}

/// 只持有领域服务，不反向捕获 Host 或窗口；设置窗口关闭即可释放页面投影。
pub struct NativeInsights {
    paths: Arc<NativePaths>,
    analytics: Arc<AnalyticsRecorder>,
    diagnostics: Arc<Diagnostics>,
    runtime: Arc<AgentRuntime>,
}

impl NativeInsights {
    pub fn new(
        paths: Arc<NativePaths>,
        analytics: Arc<AnalyticsRecorder>,
        diagnostics: Arc<Diagnostics>,
        runtime: Arc<AgentRuntime>,
    ) -> Arc<Self> {
        Arc::new(Self {
            paths,
            analytics,
            diagnostics,
            runtime,
        })
    }

    /// 文件读取和 flush 必须由设置适配器的 blocking executor 调用。
    pub fn usage(&self, query: RequestRecordsQuery) -> Result<UsageSnapshot, String> {
        let session_id = self
            .runtime
            .focused_session_id()
            .map_err(|error| error.to_string())?;
        let (stats, records, task_cache) =
            analytics::usage_stats_get(&self.paths, &self.analytics, query, session_id.as_deref())?;
        Ok(UsageSnapshot {
            stats,
            records,
            task_cache,
        })
    }

    /// 每次显式刷新采样一次；CPU 首样本可能未知，不增加常驻计时器。
    pub fn diagnostics(&self) -> DiagnosticsSnapshot {
        let resources = self.diagnostics.process_resource_sample();
        let plugin_hook_diagnostics = collect_plugin_hook_diagnostics(&self.paths.data_root);
        let store = self.diagnostics.observability();
        store.record_resource_sample(ResourceSample {
            occurred_at_ms: now_epoch_ms(),
            process_id: std::process::id(),
            cpu_percent: resources.cpu_percent,
            resident_bytes: resources.resident_bytes,
            private_bytes: resources.private_bytes,
            virtual_bytes: resources.virtual_bytes,
            process_count: resources.process_count,
        });
        if let Some(cpu) = resources.cpu_percent {
            store.record_metric("native.cpu_percent", cpu, "percent", []);
        }
        if let Some(bytes) = resources.private_bytes {
            store.record_metric("native.private_bytes", bytes as f64, "bytes", []);
        }
        DiagnosticsSnapshot {
            log_path: self.diagnostics.path().to_owned(),
            resources,
            observability: store.snapshot(),
            plugin_hook_diagnostics,
        }
    }

    pub fn export_diagnostics(&self) -> Result<String, String> {
        self.diagnostics.observability().export_redacted()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PluginHookDiagnostic, UNSUPPORTED_PLUGIN_HOOK_REASON, plugin_hook_diagnostics_from_events,
    };

    #[test]
    fn unsupported_hook_projection_contains_event_and_safe_reason() {
        let diagnostics = plugin_hook_diagnostics_from_events(
            "ely-native-hooks@local",
            ["FutureEvent".to_owned()],
        );
        assert_eq!(
            diagnostics,
            vec![PluginHookDiagnostic {
                plugin_id: "ely-native-hooks@local".to_owned(),
                event: "FutureEvent".to_owned(),
                reason: UNSUPPORTED_PLUGIN_HOOK_REASON.to_owned(),
            }]
        );
        assert!(!diagnostics[0].reason.contains("/"));
        assert_eq!(
            serde_json::to_value(&diagnostics[0]).expect("诊断应可序列化"),
            serde_json::json!({
                "plugin_id": "ely-native-hooks@local",
                "event": "FutureEvent",
                "reason": UNSUPPORTED_PLUGIN_HOOK_REASON,
            })
        );
    }

    #[test]
    fn empty_unsupported_hook_projection_is_empty() {
        assert!(
            plugin_hook_diagnostics_from_events("ely-native-hooks@local", Vec::new()).is_empty()
        );
    }
}
