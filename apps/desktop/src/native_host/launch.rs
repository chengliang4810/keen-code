//! 单一原生进程的启动与退出装配；没有旧 UI transport 或浏览器生命周期。

use anyhow::{Context, Result, bail};
use gpui::{App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use keencode_provider::ProviderRegistry;
use keencode_resources::{HostLease, HostLeaseAcquire, HostOwner};
use std::{path::PathBuf, sync::Arc, time::Instant};

use super::NativeHost;
use crate::{
    agent_runtime::{AgentRuntime, AgentRuntimeBuildConfig},
    analytics::AnalyticsRecorder,
    diagnostics::Diagnostics,
    memories::MemoryService,
    native_controls::{
        NativeControls, NativeControlsBackendCallbacks, NativeControlsCallbacks,
        NativeNotificationPump, NativeProjection, WindowFocusProbe, native_control_event_channel,
    },
    native_host::window_controls::{install_native_control_events, native_window_handle},
    native_memory::NativeMemoryCoordinator,
    native_paths::NativePaths,
    native_ui::{
        NativeAssets,
        main_workbench::NativeUi,
        settings::{
            NativeSettingsAdapter, NativeSettingsRuntimeDomain, SettingsPage, SettingsPanel,
            apply_general_settings,
        },
        workbench::panel::NativeWorkbenchPanel,
    },
    providers,
    tray::{TrayMenuLabels, TrayMenuPayload, TraySessionEntry},
    workflows::agent_tools::{WorkflowToolError, workflow_tool_port},
    workspace,
};

pub(super) fn run() -> Result<()> {
    let started = Instant::now();
    let paths = Arc::new(NativePaths::discover()?);
    crate::network_proxy::configure_before_start(&paths);
    crate::shell_env::begin_login_shell_path_capture();
    std::fs::create_dir_all(&paths.data_root).context("创建原生数据目录")?;
    // 原生应用持有唯一 OS lease；不会连接另一代 UI Host 或并行打开同一 Journal。
    let lease = match HostLease::try_acquire(&paths.data_root, HostOwner::Desktop)? {
        HostLeaseAcquire::Acquired(lease) => lease,
        HostLeaseAcquire::Busy { .. } => bail!("此数据目录已有 KeenCode 运行实例"),
    };
    let diagnostics = Diagnostics::init(&paths, started);
    diagnostics.install();
    let executor = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(8)
            .thread_name("keencode-runtime")
            .enable_all()
            .build()
            .context("创建 Rust 异步执行器")?,
    );
    let _entered = executor.enter();
    let analytics = Arc::new(AnalyticsRecorder::new(
        &paths,
        Some(diagnostics.observability()),
    )?);
    let wire_trace =
        crate::native_wire_trace::NativeWireTraceObserver::from_environment(analytics.clone())?;
    let registry = ProviderRegistry::with_request_observer(wire_trace.clone());
    let catalog = providers::list(&paths)?;
    providers::replace_runtime_registry(&registry, &catalog)?;
    let default_provider = catalog
        .active_provider_id
        .clone()
        .zip(catalog.default_model.clone());
    let app_settings = crate::app_settings::get(&paths)?;
    let memory_service = MemoryService::new(&paths)?;
    let runtime = AgentRuntime::build_native(AgentRuntimeBuildConfig {
        storage_root: paths.data_root.clone(),
        provider_registry: registry,
        analytics: Some(analytics.clone()),
        default_provider,
        memory_service: Some(memory_service.clone()),
        local_memories_enabled: app_settings.local_memories,
        executor_handle: executor.handle().clone(),
    })?;
    runtime.set_background_agent_limit(usize::from(app_settings.background_agent_limit))?;
    runtime.set_web_service_config(crate::app_settings::web_service_config(
        &app_settings.web_service_url,
    )?)?;
    let root = startup_project(&paths)?;
    let memory = NativeMemoryCoordinator::new(
        memory_service.clone(),
        runtime.clone(),
        executor.as_ref(),
        app_settings.interface_language,
    );
    memory
        .start(app_settings.local_memories)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let host = Arc::new(NativeHost::from_runtime_with_memory(
        paths.clone(),
        runtime.clone(),
        executor.clone(),
        memory.clone(),
    )?);
    executor
        .block_on(host.ensure_extension_candidate(&root))
        .map_err(anyhow::Error::msg)
        .context("构建启动项目扩展候选")?;
    let services = crate::native_services::NativeServices::new(
        paths.clone(),
        runtime.clone(),
        executor.clone(),
    );
    // Runtime 只持有弱引用，避免 Workflow AgentTool 端口与 NativeServices 形成
    // 生命周期环；每次调用仍通过同一个按 Session 缓存的真实 WorkflowHost 执行。
    let services_ref = Arc::downgrade(&services);
    runtime.set_workflow_tool_port(workflow_tool_port(move |request| {
        let services = services_ref.upgrade();
        Box::pin(async move {
            let Some(services) = services else {
                return Err(WorkflowToolError::permanent(
                    "workflow.host_unavailable",
                    "Native WorkflowHost 已释放",
                ));
            };
            services.workflow_tool_call(request)
        })
    }))?;
    let domain =
        NativeSettingsRuntimeDomain::new(paths.clone(), runtime, services.clone(), memory_service);
    let settings = NativeSettingsAdapter::new_with_operation_gate(
        paths.clone(),
        host.runtime().clone(),
        domain.clone(),
        executor.handle().clone(),
        host.inner.native_operations.clone(),
    );
    settings
        .attach_memory(&memory)
        .map_err(|error| anyhow::anyhow!(error.message))?;
    // Scheduler 的投影刷新只保存设置适配器弱引用，避免 NativeServices 与设置域形成
    // 强引用环；设置窗口关闭后后台任务仍可继续完成真实台账写入。
    services.set_settings_invalidator(&settings);
    settings.start_runtime_event_bridge();
    settings
        .attach_insights(crate::native_insights::NativeInsights::new(
            paths.clone(),
            analytics.clone(),
            diagnostics.clone(),
            host.runtime().clone(),
        ))
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let general_settings = settings
        .load_general_settings()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let keybindings_state = settings.keybindings_state();
    settings
        .load_keybindings()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    host.inner
        .settings
        .set(settings.clone())
        .map_err(|_| anyhow::anyhow!("设置服务重复装配"))?;
    host.inner
        .services
        .set(services)
        .map_err(|_| anyhow::anyhow!("运行时服务重复装配"))?;
    domain
        .start()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    diagnostics.startup_phase("native-host-ready");
    let (native_event_sender, native_event_receiver) = native_control_event_channel();
    let focus_probe = WindowFocusProbe::new();
    let trace_flush = {
        let trace = wire_trace.clone();
        Arc::new(move || trace.flush())
    };
    let backend_callbacks = NativeControlsBackendCallbacks::from_host(
        host.clone(),
        executor.clone(),
        trace_flush,
        focus_probe.clone(),
    );
    let native_callbacks =
        NativeControlsCallbacks::from_event_sender(native_event_sender, backend_callbacks);
    let shutdown_host = host.clone();
    let shutdown_diagnostics = diagnostics.clone();
    gpui_platform::application()
        .with_assets(NativeAssets)
        .run(move |cx: &mut App| {
            ely_gpui_component::init(cx);
            // 固定 Windows ZCode 参考使用灰度文字边缘；平台默认 ClearType 会产生 RGB 彩边，
            // 同一字体和字号也会显得更粗。只约束本应用渲染，不改用户的系统字体设置。
            #[cfg(target_os = "windows")]
            cx.set_text_rendering_mode(gpui::TextRenderingMode::Grayscale);
            apply_general_settings(&general_settings, cx);
            // GPUI 使用逻辑像素；按显示器可用逻辑尺寸限制初始窗口，
            // 让高 DPI 或较小屏幕上的标题和输入区保持可见。
            let display_size = cx.primary_display().map(|display| display.bounds().size);
            let width = display_size.map_or(1280.0, |size| {
                (f32::from(size.width) - 48.0).clamp(640.0, 1280.0)
            });
            let height = display_size.map_or(820.0, |size| {
                (f32::from(size.height) - 64.0).clamp(360.0, 820.0)
            });
            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("KeenCode · Ely".into()),
                    // GPUI 隐藏系统 Caption；主工作台绘制唯一标题栏及原生命中区域。
                    appears_transparent: true,
                    ..Default::default()
                }),
                window_min_size: Some(size(px(width.min(880.0)), px(height.min(560.0)))),
                // 沿用 GPUI 默认的非活动窗口动画限帧，减少后台动画唤醒。
                ..Default::default()
            };
            let ui_host = host.clone();
            let root_text = root.to_string_lossy().into_owned();
            let native_callbacks = native_callbacks;
            if let Err(error) = cx.open_window(options, move |window, cx| {
                let settings_panel = SettingsPanel::create(ui_host.settings(), cx);
                let settings_panel_for_factory = settings_panel.clone();
                let workbench_panel = NativeWorkbenchPanel::new_with_runtime(
                    ui_host.workbench(),
                    root,
                    ui_host.executor().handle().clone(),
                    cx,
                );
                let settings_navigation_panel = settings_panel.clone();
                let workbench_navigation_panel = workbench_panel.clone();
                let workbench_user_panel = workbench_panel.clone();
                let workbench_root_panel = workbench_panel.clone();
                let controls_host = ui_host.clone();
                #[cfg(windows)]
                let menu_host = ui_host.clone();
                #[cfg(windows)]
                let menu_root = root_text.clone();
                let ui = cx.new(|cx| {
                    NativeUi::new(ui_host, root_text, keybindings_state.clone(), window, cx)
                        .with_settings_panel(move |_, _| settings_panel_for_factory.element())
                        .with_settings_navigation(move |page: SettingsPage, cx| {
                            settings_navigation_panel.select_page(page, cx);
                        })
                        .with_workbench_navigation(move |pane, root, cx| {
                            workbench_navigation_panel.navigate(pane, root, cx)
                        })
                        .with_workbench_panel(move |_, _| workbench_panel.element())
                        .with_workbench_root_changed(move |root, cx| {
                            workbench_root_panel.set_root(PathBuf::from(root), cx)
                        })
                });
                // 面板由根实体持有；返回通知使用弱引用，避免回调形成 UI 强引用环。
                let ui_for_settings_back = ui.downgrade();
                settings_panel.with_back_handler(
                    move |app| {
                        let _ = ui_for_settings_back.update(app, |ui, cx| {
                            ui.native_back_to_chat(cx);
                        });
                    },
                    cx,
                );
                let ui_for_settings_navigation = ui.downgrade();
                settings_panel.with_navigation_handler(
                    move |page: SettingsPage, app| {
                        let _ = ui_for_settings_navigation.update(app, |ui, cx| {
                            ui.native_settings_page_selected(page, cx);
                        });
                    },
                    cx,
                );
                let ui_for_workbench_navigation = ui.downgrade();
                workbench_user_panel.with_navigation_handler(
                    move |pane, root, app| {
                        let _ = ui_for_workbench_navigation.update(app, |ui, cx| {
                            ui.native_workbench_pane_selected(pane, root, cx);
                        });
                    },
                    cx,
                );
                let window_handle = match native_window_handle(window) {
                    Ok(handle) => handle,
                    Err(error) => {
                        tracing::error!(%error, "读取 Native GPUI 窗口句柄失败");
                        cx.quit();
                        return ui;
                    }
                };
                let controls = NativeControls::new(
                    controls_host.native_paths(),
                    controls_host.runtime().clone(),
                    native_callbacks,
                    window_handle,
                    keybindings_state.clone(),
                );
                focus_probe.set_window(window_handle);
                if let Err(error) = settings.attach_update_port(controls.clone()) {
                    tracing::error!(message = %error.message, "装配原生更新设置端口失败");
                }
                if let Err(error) = install_native_control_events(
                    Arc::clone(&controls),
                    native_event_receiver,
                    ui.clone(),
                    window,
                    cx,
                ) {
                    tracing::error!(%error, "安装 Native 窗口控制失败");
                    cx.quit();
                    return ui;
                }
                #[cfg(windows)]
                let start_result =
                    controls.start(initial_tray_menu(menu_host.as_ref(), &menu_root));
                #[cfg(not(windows))]
                let start_result = controls.start();
                if let Err(error) = start_result {
                    tracing::error!(%error, "安装 Native 托盘失败");
                    cx.quit();
                }
                let projection_host = Arc::downgrade(&controls_host);
                let projection = Arc::new(move || {
                    let host = projection_host
                        .upgrade()
                        .ok_or_else(|| "原生宿主已释放".to_owned())?;
                    current_native_projection(&host)
                });
                let pump = NativeNotificationPump::start(
                    controls_host.runtime().clone(),
                    controls_host.executor().clone(),
                    controls.clone(),
                    projection,
                );
                if controls_host.inner.notification_pump.set(pump).is_err() {
                    tracing::error!("原生通知观察者重复装配");
                }
                ui
            }) {
                diagnostics.error("native-window", error.to_string());
                cx.quit();
                return;
            }
            diagnostics.startup_phase("native-window-ready");
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
    executor.block_on(shutdown_host.shutdown())?;
    analytics.flush().map_err(anyhow::Error::msg)?;
    wire_trace.flush().map_err(anyhow::Error::msg)?;
    shutdown_diagnostics.flush().map_err(anyhow::Error::msg)?;
    drop(analytics);
    drop(lease);
    Ok(())
}

fn startup_project(paths: &NativePaths) -> Result<PathBuf> {
    // 验收只设置隔离工作区，不提供调用业务动作或模拟模型完成的隐藏通道。
    if std::env::var_os("KEENCODE_BENCHMARK").as_deref() == Some(std::ffi::OsStr::new("1"))
        && std::env::var_os("KEENCODE_NATIVE_ACCEPTANCE").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        && let Some(root) = std::env::var_os("KEENCODE_NATIVE_ACCEPTANCE_PROJECT")
    {
        let root = std::fs::canonicalize(PathBuf::from(root)).context("验收项目不存在")?;
        register_startup_project(paths, &root, "原生验收")?;
        return Ok(root);
    }
    if let Some(project) = workspace::project_records(paths)
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .find(|project| PathBuf::from(&project.path).is_dir())
    {
        return std::fs::canonicalize(project.path).context("读取最近项目目录");
    }
    let root = workspace::conversation_workspace_root(paths).map_err(anyhow::Error::msg)?;
    register_startup_project(paths, &root, "默认项目")?;
    Ok(root)
}

fn register_startup_project(paths: &NativePaths, root: &std::path::Path, name: &str) -> Result<()> {
    let root_text = root.to_string_lossy();
    // 冷启动复用已经登记的目录；空数据首次启动也必须让默认会话可从侧栏重新打开。
    if !workspace::project_records(paths)
        .map_err(anyhow::Error::msg)?
        .iter()
        .any(|project| super::actions::same_path(&project.path, &root_text))
    {
        workspace::project_create(paths, Some(root_text.into_owned()), name.into(), false)
            .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

#[cfg(windows)]
fn initial_tray_menu(host: &NativeHost, root_path: &str) -> TrayMenuPayload {
    let sessions = host
        .workspace_page(root_path, None, false)
        .map(|page| {
            page.sessions
                .into_iter()
                .map(|session| TraySessionEntry {
                    id: session.session_id,
                    title: session.title,
                })
                .collect()
        })
        .unwrap_or_default();
    TrayMenuPayload {
        labels: TrayMenuLabels {
            new_chat: "新建对话".to_owned(),
            show: "显示窗口".to_owned(),
            quit: "退出 KeenCode".to_owned(),
        },
        sessions,
    }
}

fn current_native_projection(host: &NativeHost) -> Result<NativeProjection, String> {
    // 侧栏覆盖所有已登记项目；删除初始项目后通知投影仍可读取剩余会话。
    let page = host
        .workspace_page("", None, false)
        .map_err(|error| error.message)?;
    let unread_count = page.projects.iter().fold(0u32, |sum, project| {
        sum.saturating_add(u32::try_from(project.unread_count).unwrap_or(u32::MAX))
    });
    Ok(NativeProjection {
        unread_count,
        menu: TrayMenuPayload {
            labels: TrayMenuLabels {
                new_chat: "新建对话".into(),
                show: "显示窗口".into(),
                quit: "退出 KeenCode".into(),
            },
            sessions: page
                .sessions
                .into_iter()
                .map(|session| TraySessionEntry {
                    id: session.session_id,
                    title: session.title,
                })
                .collect(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_start_reuses_registered_project_without_renaming() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let paths = NativePaths::from_data_root(storage.path().to_owned());
        register_startup_project(&paths, project.path(), "首次登记").unwrap();
        register_startup_project(&paths, project.path(), "冷启动默认名").unwrap();
        let records = workspace::project_records(&paths).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "首次登记");
        assert!(super::super::actions::same_path(
            &records[0].path,
            &project.path().to_string_lossy()
        ));
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use crate::native_paths::NativePaths;

    #[test]
    fn startup_project_registers_default_root_once() {
        let storage = tempfile::tempdir().expect("启动测试存储目录应创建");
        let paths = NativePaths::from_data_root(storage.path().to_path_buf());

        let first_root = startup_project(&paths).expect("空数据首次启动应创建默认项目");
        let first_projects = workspace::project_records(&paths).expect("首次项目记录应可读取");
        assert_eq!(first_projects.len(), 1);
        assert!(crate::native_host::actions::same_path(
            &first_projects[0].path,
            &first_root.to_string_lossy(),
        ));

        let second_root = startup_project(&paths).expect("重复启动应复用默认项目");
        let second_projects = workspace::project_records(&paths).expect("重复项目记录应可读取");
        assert_eq!(second_root, first_root);
        assert_eq!(second_projects.len(), 1);
        assert!(crate::native_host::actions::same_path(
            &second_projects[0].path,
            &second_root.to_string_lossy(),
        ));
    }
}
