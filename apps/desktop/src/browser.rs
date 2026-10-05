//! 右侧资源面板的内置浏览器。
//!
//! 每个网页标签对应一个独立原生子 WebView：切换标签只切换显隐，因此各标签的
//! 滚动位置、表单内容和前端路由在切换后保留。
//!
//! 子 WebView 由系统原生层绘制，始终覆盖在主 WebView 之上，主界面的下拉菜单、
//! 弹窗和右键菜单无法盖住它；前端在浮层出现时必须隐藏浏览器。
//!
//! 命令只接受完整 URL：地址补全与本地路径转换由前端纯规则完成。

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt::Write as FmtWrite,
    sync::atomic::{AtomicU64, Ordering},
    sync::{Mutex, OnceLock},
};
pub mod screenshot;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, WebviewUrl,
    webview::{PageLoadEvent, Webview, WebviewBuilder},
};

/// 子 WebView 导航与标题变化回写主界面的统一事件名。
const BROWSER_STATE_EVENT: &str = "browser://state";
/// 子 WebView label 前缀，避免与主 WebView 及其他 Tauri label 冲突。
const LABEL_PREFIX: &str = "browser-";
/// 主窗口 label；子 WebView 归属该窗口，状态事件也发给它。
const MAIN_WINDOW: &str = "main";
/// 子 WebView 的初始地址。
///
/// 必须用空白页而不是目标地址创建：Tauri 按 WebView 创建时的地址推导来源，并把
/// `asset://` 本地文件协议的 `Access-Control-Allow-Origin` 设成同一来源。若直接用
/// 远程地址创建，远程页面就获得读取 `assetProtocol.scope`（含 `$HOME/**`）的能力。
/// 空白页来源为 `null`，远程页面无法通过该协议读取本机文件。
const BLANK_PAGE: &str = "about:blank";

static NEXT_NATIVE_GENERATION: AtomicU64 = AtomicU64::new(1);
// `browser_open` 会跨多个 await 完成 child 创建；闸门避免同一 tab 的首个请求尚未
// 回写 Webview 时，第二个请求误判为“未创建”并再次 add_child。
static BROWSER_OPEN_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static NEXT_BROWSER_OPEN_ATTEMPT: AtomicU64 = AtomicU64::new(1);
static BROWSER_IDENTITIES: OnceLock<Mutex<HashMap<String, BrowserIdentity>>> = OnceLock::new();

fn browser_identities() -> &'static Mutex<HashMap<String, BrowserIdentity>> {
    BROWSER_IDENTITIES.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserOwner {
    workspace_key: String,
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    browser_generation: Option<u64>,
}

impl BrowserOwner {
    fn validate(&self) -> Result<(), String> {
        if self.workspace_key.trim().is_empty() || self.session_id.trim().is_empty() {
            return Err("浏览器 owner 必须包含 workspace/session".to_owned());
        }
        if self.workspace_key.len() > 512 || self.session_id.len() > 512 {
            return Err("浏览器 owner 字段过长".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BrowserIdentity {
    owner: BrowserOwner,
    generation: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserOpenResult {
    generation: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserState {
    /// 网页标签标识，与前端标签 id 一致（不含 label 前缀）。
    tab_id: String,
    /// child 创建时绑定的业务 owner；事件不能跨 workspace/session 消费。
    owner: BrowserOwner,
    /// Rust 分配的不可复用 child 代次；旧 child 的迟到回调不会通过校验。
    generation: u64,
    /// 当前页面地址。
    url: String,
    /// 页面标题；仅标题变化事件携带。
    title: Option<String>,
    /// 事件类别：`started`、`finished`、`title` 或请求原界面新标签的 `new-window`。
    kind: &'static str,
}

/// 生成注入式且可逆的子 WebView label。
///
/// 逐 UTF-8 字节编码为十六进制，避免把不同 tab id（例如 `a b` 与 `a-b`）
/// 映射成同一个 Tauri label；同时只使用 Tauri label 的安全字符。
fn label_for(tab_id: &str) -> String {
    let mut encoded = String::with_capacity(tab_id.len() * 2);
    for byte in tab_id.as_bytes() {
        let _ = write!(encoded, "{byte:02x}");
    }
    format!("{LABEL_PREFIX}{encoded}")
}

/// 解析并校验子 WebView 的加载地址。
///
/// 只接受 http/https，以及创建子 WebView 用的空白页。地址栏里的本地路径由前端交给
/// 文件预览链路（本地 HTML 用 `srcDoc` 渲染），因此这里不接受 `file:`、`data:` 或
/// `javascript:` 等协议。
fn parse_url(raw: &str) -> Result<Url, String> {
    let trimmed = raw.trim();
    if trimmed == BLANK_PAGE {
        return Url::parse(BLANK_PAGE).map_err(|error| format!("浏览器初始地址无效：{error}"));
    }
    let url = Url::parse(trimmed).map_err(|error| format!("浏览器地址无效：{error}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(format!("内置浏览器不支持该协议：{other}")),
    }
}

fn is_main_webview_identity(webview_label: &str, window_label: &str) -> bool {
    webview_label == MAIN_WINDOW && window_label == MAIN_WINDOW
}

fn require_main_webview(webview: &Webview) -> Result<(), String> {
    // Tauri 的 child WebView 与主界面共享同一个宿主 Window；只校验 Window label
    // 会把外部网页的 IPC 误当成主界面命令，因此必须同时核对 caller WebView label。
    if is_main_webview_identity(webview.label(), webview.window().label()) {
        Ok(())
    } else {
        Err("内置浏览器命令只允许主窗口调用".to_owned())
    }
}

fn require_webview(
    app: &AppHandle,
    caller: &Webview,
    tab_id: &str,
    owner: &BrowserOwner,
    generation: u64,
) -> Result<tauri::Webview, String> {
    require_main_webview(caller)?;
    owner.validate()?;
    let expected = BrowserIdentity {
        owner: owner.clone(),
        generation,
    };
    let current = browser_identities()
        .lock()
        .map_err(|_| "浏览器状态锁已损坏".to_owned())?
        .get(tab_id)
        .cloned()
        .ok_or_else(|| "浏览器标签已关闭".to_owned())?;
    if current != expected {
        return Err("浏览器标签代次或 owner 已失效".to_owned());
    }
    app.get_webview(&label_for(tab_id))
        .ok_or_else(|| "网页标签不存在".to_string())
}

/// 把标签的显示区域对齐到前端宿主元素。
fn apply_bounds(
    webview: &tauri::Webview,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    webview
        .set_position(LogicalPosition::new(x, y))
        .and_then(|()| webview.set_size(LogicalSize::new(width, height)))
        .map_err(|error| format!("设置浏览器区域失败：{error}"))
}

/// Rust 侧再次保证单可见 child；前端的显隐队列即使交错，也不会把两个原生表面
/// 同时留在主窗口上。
fn hide_other_browser_webviews(app: &AppHandle, active_tab_id: &str) -> Result<(), String> {
    let tab_ids = browser_identities()
        .lock()
        .map_err(|_| "浏览器状态锁已损坏".to_owned())?
        .keys()
        .filter(|tab_id| tab_id.as_str() != active_tab_id)
        .cloned()
        .collect::<Vec<_>>();
    for tab_id in tab_ids {
        if let Some(webview) = app.get_webview(&label_for(&tab_id)) {
            webview
                .hide()
                .map_err(|error| format!("隐藏其他浏览器标签失败：{error}"))?;
        }
    }
    Ok(())
}

fn emit_state(
    app: &AppHandle,
    tab_id: &str,
    identity: &BrowserIdentity,
    url: &str,
    title: Option<String>,
    kind: &'static str,
) {
    let is_current = browser_identities()
        .lock()
        .map(|states| identity_matches(states.get(tab_id), identity))
        .unwrap_or(false);
    if !is_current {
        tracing::debug!(
            target: "keencode_diagnostics",
            stage = "browser_state_dropped_stale",
            kind,
            tab_id_len = tab_id.len(),
            generation = identity.generation,
            "browser state callback rejected by owner/generation registry"
        );
        return;
    }
    let result = app.emit_to(
        MAIN_WINDOW,
        BROWSER_STATE_EVENT,
        BrowserState {
            tab_id: tab_id.to_string(),
            owner: identity.owner.clone(),
            generation: identity.generation,
            url: url.to_string(),
            title,
            kind,
        },
    );
    match result {
        Ok(()) => tracing::info!(
            target: "keencode_diagnostics",
            stage = "browser_state_emitted",
            kind,
            tab_id_len = tab_id.len(),
            generation = identity.generation,
            url_len = url.len(),
            "browser state event emitted to main window"
        ),
        Err(error) => tracing::warn!(
            target: "keencode_diagnostics",
            stage = "browser_state_emit_failed",
            kind,
            tab_id_len = tab_id.len(),
            generation = identity.generation,
            url_len = url.len(),
            error = %error,
            "browser state event emit failed"
        ),
    }
}

fn identity_matches(current: Option<&BrowserIdentity>, expected: &BrowserIdentity) -> bool {
    current == Some(expected)
}

/// 创建或复用网页标签的子 WebView，并加载目标地址。
///
/// 创建时的界面逻辑坐标，以单个边界结构传入，避免分散的坐标参数。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserOpenBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// 创建走异步命令：Windows 上在同步命令里创建 WebView 会死锁。
#[tauri::command]
pub async fn browser_open(
    app: AppHandle,
    caller: Webview,
    tab_id: String,
    url: String,
    bounds: BrowserOpenBounds,
    owner: BrowserOwner,
) -> Result<BrowserOpenResult, String> {
    require_main_webview(&caller)?;
    if tab_id.trim().is_empty() {
        return Err("浏览器 tab id 不能为空".to_owned());
    }
    let BrowserOpenBounds {
        x,
        y,
        width,
        height,
    } = bounds;
    owner.validate()?;
    let target = parse_url(&url)?;
    let label = label_for(&tab_id);
    let tab_id_len = tab_id.len();
    let open_attempt = NEXT_BROWSER_OPEN_ATTEMPT.fetch_add(1, Ordering::Relaxed);
    let _open_gate = BROWSER_OPEN_GATE.lock().await;
    let registry_size = browser_identities()
        .lock()
        .map_err(|_| "浏览器状态锁已损坏".to_owned())?
        .len();
    tracing::info!(
        target: "keencode_diagnostics",
        stage = "browser_open.request",
        attempt = open_attempt,
        tab_id_len,
        registry_size,
        "browser open request entered serialized gate"
    );

    // 同一 owner 的地址栏提交复用 child；owner 变化时先使旧代次失效，
    // 旧 callback 即使稍后抵达也不能再更新新 tab。
    let current = browser_identities()
        .lock()
        .map_err(|_| "浏览器状态锁已损坏".to_owned())?
        .get(&tab_id)
        .cloned();
    if let Some(identity) = current.as_ref()
        && identity.owner == owner
        && let Some(webview) = app.get_webview(&label)
    {
        apply_bounds(&webview, x, y, width, height)?;
        webview
            .navigate(target)
            .map_err(|error| format!("加载浏览器地址失败：{error}"))?;
        tracing::info!(
            target: "keencode_diagnostics",
            stage = "browser_open.reuse",
            attempt = open_attempt,
            tab_id_len,
            generation = identity.generation,
            registry_size,
            "browser child reused"
        );
        return Ok(BrowserOpenResult {
            generation: identity.generation,
        });
    }
    if current.is_some() {
        browser_identities()
            .lock()
            .map_err(|_| "浏览器状态锁已损坏".to_owned())?
            .remove(&tab_id);
        if let Some(webview) = app.get_webview(&label) {
            webview
                .close()
                .map_err(|error| format!("关闭旧浏览器标签失败：{error}"))?;
        }
    } else if let Some(webview) = app.get_webview(&label) {
        // 热重载可能留下没有登记表项的 child；不能把未知 child 复用给新 owner。
        webview
            .close()
            .map_err(|error| format!("关闭未登记浏览器标签失败：{error}"))?;
    }

    let window = app
        .get_window(MAIN_WINDOW)
        .ok_or_else(|| "主窗口不存在".to_string())?;

    let blank = Url::parse(BLANK_PAGE).map_err(|error| format!("浏览器初始地址无效：{error}"))?;

    let load_app = app.clone();
    let load_tab = tab_id.clone();
    let load_identity = BrowserIdentity {
        owner: owner.clone(),
        generation: NEXT_NATIVE_GENERATION.fetch_add(1, Ordering::Relaxed),
    };
    let native_generation = load_identity.generation;
    let load_callback_identity = load_identity.clone();
    browser_identities()
        .lock()
        .map_err(|_| "浏览器状态锁已损坏".to_owned())?
        .insert(tab_id.clone(), load_identity.clone());
    let title_app = app.clone();
    let title_tab = tab_id.clone();
    let title_identity = load_identity.clone();
    let popup_app = app.clone();
    let popup_tab = tab_id.clone();
    let popup_identity = load_identity.clone();

    let builder = WebviewBuilder::new(label, WebviewUrl::External(blank))
        .on_page_load(move |_, payload| {
            let kind = match payload.event() {
                PageLoadEvent::Started => "started",
                PageLoadEvent::Finished => "finished",
            };
            emit_state(
                &load_app,
                &load_tab,
                &load_callback_identity,
                payload.url().as_str(),
                None,
                kind,
            );
        })
        .on_document_title_changed(move |webview, title| {
            let url = webview.url().map(|u| u.to_string()).unwrap_or_default();
            emit_state(
                &title_app,
                &title_tab,
                &title_identity,
                &url,
                Some(title),
                "title",
            );
        })
        .on_new_window(move |url, _features| {
            // 网页不能创建不受主界面生命周期管理的原生窗口。只转发允许的
            // 地址，由现有浏览器 tab 状态所有者创建同布局的 child WebView；
            // javascript/file/data 和隐藏标签的弹窗不会获得系统窗口能力。
            if matches!(url.scheme(), "http" | "https") {
                tracing::info!(
                    target: "keencode_diagnostics",
                    stage = "browser_new_window_received",
                    tab_id_len = popup_tab.len(),
                    generation = popup_identity.generation,
                    url_len = url.as_str().len(),
                    "browser child received an allowed target-blank navigation"
                );
                emit_state(
                    &popup_app,
                    &popup_tab,
                    &popup_identity,
                    url.as_str(),
                    None,
                    "new-window",
                );
            }
            tauri::webview::NewWindowResponse::Deny
        });

    let child_tab_id = tab_id.clone();
    let child_identity = load_identity.clone();
    let cleanup = || -> Result<(), String> {
        let mut states = browser_identities()
            .lock()
            .map_err(|_| "浏览器状态锁已损坏".to_owned())?;
        if states.get(&child_tab_id) == Some(&child_identity) {
            states.remove(&child_tab_id);
        }
        Ok(())
    };

    // `with_webview` 的回调运行在主 WebView2 STA；`add_child` 内部还会同步调度
    // 一次主线程并等待结果，因此不能在该回调里调用 `add_child`，否则会阻塞同一
    // UI 线程，表现为地址栏已更新但子 WebView 永远停在空白页。回调只取得
    // Tauri 官方 Send/Sync 边界中的 builder，返回后再创建 child，避免裸 COM
    // 引用经过通道或在取消路径跨线程析构。
    let builder_slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let callback_builder_slot = std::sync::Arc::clone(&builder_slot);
    let (builder_ready_send, builder_ready_receive) = tokio::sync::oneshot::channel::<()>();
    let callback_result = caller.with_webview(move |caller_webview| {
        #[cfg(windows)]
        let builder = builder.with_environment(caller_webview.environment());

        #[cfg(not(windows))]
        let builder = builder;

        if let Ok(mut slot) = callback_builder_slot.lock() {
            slot.replace(builder);
            tracing::info!(
                target: "keencode_diagnostics",
                stage = "browser_open.environment_ready",
                attempt = open_attempt,
                tab_id_len,
                generation = native_generation,
                registry_size,
                "browser child environment captured"
            );
        } else {
            tracing::warn!(
                target: "keencode_diagnostics",
                stage = "browser_open.environment_slot_failed",
                attempt = open_attempt,
                tab_id_len,
                generation = native_generation,
                registry_size,
                "browser child environment slot is poisoned"
            );
        }
        let _ = builder_ready_send.send(());
    });
    if let Err(error) = callback_result {
        let _ = cleanup();
        return Err(format!("读取主 WebView2 环境失败：{error}"));
    }
    if tokio::time::timeout(std::time::Duration::from_secs(5), builder_ready_receive)
        .await
        .is_err()
    {
        let _ = cleanup();
        tracing::warn!(
            target: "keencode_diagnostics",
            stage = "browser_open.environment_timeout",
            attempt = open_attempt,
            tab_id_len,
            generation = native_generation,
            registry_size,
            "browser child environment callback did not complete"
        );
        return Err("读取主 WebView2 环境超时".to_owned());
    }
    let builder = match builder_slot.lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => None,
    };
    let Some(builder) = builder else {
        let _ = cleanup();
        return Err("读取主 WebView2 环境失败：builder 不可用".to_owned());
    };

    tracing::info!(
        target: "keencode_diagnostics",
        stage = "browser_open.add_child_started",
        attempt = open_attempt,
        tab_id_len,
        generation = native_generation,
        registry_size,
        "creating browser child webview"
    );
    // `Window::add_child` 会在内部把构建任务投递到主线程后同步等待结果。即使
    // 当前命令声明为 async，也不能假定 Tauri 的异步运行时一定使用工作线程；
    // 把这个同步等待放入 blocking 线程，避免它占住负责分发主线程任务的执行器。
    let add_child_task = tokio::task::spawn_blocking(move || {
        window.add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(width, height),
        )
    });
    let webview =
        match tokio::time::timeout(std::time::Duration::from_secs(10), add_child_task).await {
            Err(_) => {
                let _ = cleanup();
                tracing::warn!(
                    target: "keencode_diagnostics",
                    stage = "browser_open.add_child_timeout",
                    attempt = open_attempt,
                    tab_id_len,
                    generation = native_generation,
                    registry_size,
                    "browser child webview creation timed out"
                );
                return Err("创建内置浏览器超时".to_owned());
            }
            Ok(Err(error)) => {
                let _ = cleanup();
                tracing::warn!(
                    target: "keencode_diagnostics",
                    stage = "browser_open.add_child_join_failed",
                    attempt = open_attempt,
                    tab_id_len,
                    generation = native_generation,
                    registry_size,
                    "browser child webview creation task failed"
                );
                return Err(format!("创建内置浏览器任务失败：{error}"));
            }
            Ok(Ok(Err(error))) => {
                let _ = cleanup();
                tracing::warn!(
                    target: "keencode_diagnostics",
                    stage = "browser_open.add_child_failed",
                    attempt = open_attempt,
                    tab_id_len,
                    generation = native_generation,
                    registry_size,
                    "browser child webview creation failed"
                );
                return Err(format!("创建内置浏览器失败：{error}"));
            }
            Ok(Ok(Ok(webview))) => webview,
        };
    tracing::info!(
        target: "keencode_diagnostics",
        stage = "browser_open.add_child_completed",
        attempt = open_attempt,
        tab_id_len,
        generation = native_generation,
        registry_size,
        "browser child webview created"
    );

    // 来源浏览器不提供旧标注浮层；child 只加载页面，不再注入已退役前端脚本。
    if let Err(error) = webview.navigate(target) {
        let _ = webview.close();
        let cleanup_result = cleanup();
        tracing::warn!(
            target: "keencode_diagnostics",
            stage = "browser_open.navigate_failed",
            attempt = open_attempt,
            tab_id_len,
            generation = native_generation,
            registry_size,
            "browser child initial navigation failed"
        );
        return match cleanup_result {
            Ok(()) => Err(format!("加载浏览器地址失败：{error}")),
            Err(cleanup_error) => Err(cleanup_error),
        };
    }
    tracing::info!(
        target: "keencode_diagnostics",
        stage = "browser_open.navigate_completed",
        attempt = open_attempt,
        tab_id_len,
        generation = native_generation,
        registry_size,
        "browser child initial navigation scheduled"
    );

    // 不在这里显示：非激活标签由前端显隐命令控制，避免创建时闪一下。
    Ok(BrowserOpenResult {
        generation: native_generation,
    })
}

/// 更新网页标签的显示区域（面板尺寸或分隔条变化时调用）。
#[tauri::command]
pub fn browser_bounds(
    app: AppHandle,
    caller: Webview,
    tab_id: String,
    owner: BrowserOwner,
    generation: u64,
    bounds: BrowserOpenBounds,
) -> Result<(), String> {
    let BrowserOpenBounds {
        x,
        y,
        width,
        height,
    } = bounds;
    apply_bounds(
        &require_webview(&app, &caller, &tab_id, &owner, generation)?,
        x,
        y,
        width,
        height,
    )
}

/// 显示网页标签；同一时刻只有当前激活标签可见。
#[tauri::command]
pub fn browser_show(
    app: AppHandle,
    caller: Webview,
    tab_id: String,
    owner: BrowserOwner,
    generation: u64,
) -> Result<(), String> {
    let webview = require_webview(&app, &caller, &tab_id, &owner, generation)?;
    hide_other_browser_webviews(&app, &tab_id)?;
    webview
        .show()
        .map_err(|error| format!("显示内置浏览器失败：{error}"))
}

/// 隐藏网页标签（切换标签、收起面板或浮层出现时调用）。
#[tauri::command]
pub fn browser_hide(
    app: AppHandle,
    caller: Webview,
    tab_id: String,
    owner: BrowserOwner,
    generation: u64,
) -> Result<(), String> {
    require_webview(&app, &caller, &tab_id, &owner, generation)?
        .hide()
        .map_err(|error| format!("隐藏内置浏览器失败：{error}"))
}

/// 关闭网页标签并释放其 WebView。
#[tauri::command]
pub fn browser_close(
    app: AppHandle,
    caller: Webview,
    tab_id: String,
    owner: BrowserOwner,
    generation: u64,
) -> Result<(), String> {
    require_main_webview(&caller)?;
    owner.validate()?;
    let expected = BrowserIdentity { owner, generation };
    let current = browser_identities()
        .lock()
        .map_err(|_| "浏览器状态锁已损坏".to_owned())?
        .get(&tab_id)
        .cloned();
    let Some(current) = current else {
        return Ok(());
    };
    if current != expected {
        return Err("浏览器标签代次或 owner 已失效".to_owned());
    }
    let Some(webview) = app.get_webview(&label_for(&tab_id)) else {
        browser_identities()
            .lock()
            .map_err(|_| "浏览器状态锁已损坏".to_owned())?
            .remove(&tab_id);
        return Ok(());
    };
    // 先从 registry 撤销 callback，再等待 close；旧页面的迟到事件不能在关闭期间
    // 写回同 tabId 后续创建的新 child。
    browser_identities()
        .lock()
        .map_err(|_| "浏览器状态锁已损坏".to_owned())?
        .remove(&tab_id);
    if let Err(error) = webview.close() {
        browser_identities()
            .lock()
            .map_err(|_| "浏览器状态锁已损坏".to_owned())?
            .insert(tab_id, expected);
        return Err(format!("关闭内置浏览器失败：{error}"));
    }
    Ok(())
}

/// 在已有标签内导航到新地址。
#[tauri::command]
pub fn browser_navigate(
    app: AppHandle,
    caller: Webview,
    tab_id: String,
    owner: BrowserOwner,
    generation: u64,
    url: String,
) -> Result<(), String> {
    let target = parse_url(&url)?;
    require_webview(&app, &caller, &tab_id, &owner, generation)?
        .navigate(target)
        .map_err(|error| format!("加载浏览器地址失败：{error}"))
}

/// 重新加载当前页面。
#[tauri::command]
pub fn browser_reload(
    app: AppHandle,
    caller: Webview,
    tab_id: String,
    owner: BrowserOwner,
    generation: u64,
) -> Result<(), String> {
    require_webview(&app, &caller, &tab_id, &owner, generation)?
        .reload()
        .map_err(|error| format!("重新加载页面失败：{error}"))
}

/// 缩放原生网页内容，不缩放主界面或只改变宿主元素尺寸。
/// 原工具栏使用 25%–500%；拒绝非有限数值，避免把无效值传给 WebView2。
#[tauri::command]
pub fn browser_zoom(
    app: AppHandle,
    caller: Webview,
    tab_id: String,
    owner: BrowserOwner,
    generation: u64,
    zoom: f64,
) -> Result<(), String> {
    if !zoom.is_finite() || !(0.25..=5.0).contains(&zoom) {
        return Err("浏览器缩放比例必须在 25% 到 500% 之间".to_owned());
    }
    require_webview(&app, &caller, &tab_id, &owner, generation)?
        .set_zoom(zoom)
        .map_err(|error| format!("设置浏览器缩放失败：{error}"))
}

/// 在当前标签的浏览历史中后退或前进。
#[tauri::command]
pub fn browser_history(
    app: AppHandle,
    caller: Webview,
    tab_id: String,
    owner: BrowserOwner,
    generation: u64,
    direction: String,
) -> Result<(), String> {
    let script = match direction.as_str() {
        "back" => "history.back()",
        "forward" => "history.forward()",
        other => return Err(format!("不支持的历史方向：{other}")),
    };
    require_webview(&app, &caller, &tab_id, &owner, generation)?
        .eval(script)
        .map_err(|error| format!("切换浏览历史失败：{error}"))
}

/// 从真实 WebView2 历史读取按钮能力；不能用页面 URL 列表推测原生历史。
/// 主页面重载会保留 Tauri 子 WebView，重建适配实例时清理上次的可丢弃浏览器窗口。
#[tauri::command]
pub fn browser_reset(app: AppHandle, caller: Webview) -> Result<(), String> {
    require_main_webview(&caller)?;
    // 先使所有旧 callback 失效，再等待每个 child close；关闭期间抵达的页面事件
    // 不能回填即将重建的 renderer。
    browser_identities()
        .lock()
        .map_err(|_| "浏览器状态锁已损坏".to_owned())?
        .clear();
    for (label, webview) in app.webviews() {
        if label.starts_with(LABEL_PREFIX) {
            webview
                .close()
                .map_err(|e| format!("清理浏览器窗口失败：{e}"))?;
        }
    }
    Ok(())
}

/// 从真实 WebView2 历史读取按钮能力；不能用页面 URL 列表推测原生历史。
#[tauri::command]
pub async fn browser_navigation_state(
    app: AppHandle,
    caller: Webview,
    tab_id: String,
    owner: BrowserOwner,
    generation: u64,
) -> Result<serde_json::Value, String> {
    let webview = require_webview(&app, &caller, &tab_id, &owner, generation)?;
    #[cfg(windows)]
    {
        let (send, receive) = tokio::sync::oneshot::channel();
        webview.with_webview(move |native| {
            // COM 调用只在 Tauri 调度的 WebView 线程执行，BOOL 内存由本作用域持有。
            let result = unsafe {
                (|| {
                    let core = native.controller().CoreWebView2().map_err(|e| e.to_string())?;
                    let mut back = Default::default(); let mut forward = Default::default();
                    core.CanGoBack(&mut back).map_err(|e| e.to_string())?;
                    core.CanGoForward(&mut forward).map_err(|e| e.to_string())?;
                    Ok::<_, String>(serde_json::json!({"canGoBack":back.as_bool(), "canGoForward":forward.as_bool()}))
                })()
            };
            let _ = send.send(result);
        }).map_err(|e| e.to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(3), receive)
            .await
            .map_err(|_| "读取浏览器历史超时".to_owned())?
            .map_err(|_| "浏览器已关闭".to_owned())?
    }
    #[cfg(not(windows))]
    {
        let _ = webview;
        Err("当前平台尚未对接原生浏览器历史查询".to_owned())
    }
}

/// 原生验收读取 WebView2 控制器的真实显隐与区域；DOM 占位层不能证明原生表面已隐藏。
/// 只在显式隔离的 native-desktop-tests 运行中开放，不参与产品业务状态或截图合成。
#[tauri::command]
pub async fn browser_surface_state(
    app: AppHandle,
    caller: Webview,
) -> Result<Vec<serde_json::Value>, String> {
    require_main_webview(&caller)?;
    if !cfg!(feature = "native-desktop-tests")
        || std::env::var("KEENCODE_BENCHMARK").as_deref() != Ok("1")
        || std::env::var_os("KEENCODE_BENCHMARK_DATA_DIR").is_none()
    {
        return Err("原生表面取证只允许隔离验收主界面".to_owned());
    }
    #[cfg(windows)]
    {
        let mut states = Vec::new();
        let identities = browser_identities()
            .lock()
            .map_err(|_| "浏览器状态锁已损坏".to_owned())?
            .clone();
        for (tab_id, identity) in identities {
            let Some(child) = app.get_webview(&label_for(&tab_id)) else {
                continue;
            };
            let position = child.position().map_err(|error| error.to_string())?;
            let size = child.size().map_err(|error| error.to_string())?;
            let url = child.url().map_err(|error| error.to_string())?;
            let (send, receive) = tokio::sync::oneshot::channel();
            child
                .with_webview(move |native| {
                    // COM 控制器只在 Tauri 调度的所属线程读取；没有跨线程持有 COM 对象。
                    let result = unsafe {
                        let mut visible = Default::default();
                        native
                            .controller()
                            .IsVisible(&mut visible)
                            .map(|()| visible.as_bool())
                            .map_err(|error| error.to_string())
                    };
                    let _ = send.send(result);
                })
                .map_err(|error| error.to_string())?;
            let visible = tokio::time::timeout(std::time::Duration::from_secs(3), receive)
                .await
                .map_err(|_| "读取原生表面显隐超时".to_owned())?
                .map_err(|_| "原生表面已关闭".to_owned())??;
            states.push(serde_json::json!({
                "tabId": tab_id,
                "owner": identity.owner,
                "generation": identity.generation,
                "url": url.as_str(), "visible": visible,
                "physicalBounds": {"x":position.x,"y":position.y,"width":size.width,"height":size.height}
            }));
        }
        states.sort_by(|left, right| left["tabId"].as_str().cmp(&right["tabId"].as_str()));
        Ok(states)
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Err("当前验收入口仅支持 WebView2".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_webview_identity_rejects_child_webview_on_same_window() {
        assert!(is_main_webview_identity(MAIN_WINDOW, MAIN_WINDOW));
        assert!(!is_main_webview_identity("browser-746162", MAIN_WINDOW));
        assert!(!is_main_webview_identity(MAIN_WINDOW, "secondary"));
    }

    #[test]
    fn label_encoding_is_injective_and_uses_safe_characters() {
        assert_eq!(
            label_for("tab_1758_ab12"),
            "browser-7461625f313735385f61623132"
        );
        assert_ne!(label_for("a b"), label_for("a-b"));
        assert_ne!(label_for("a/b"), label_for("a:b"));
        assert!(
            label_for("a b#c")
                .strip_prefix(LABEL_PREFIX)
                .unwrap()
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        );
        // 前缀保证不会与主 WebView 的 label 冲突。
        assert!(label_for("main").starts_with(LABEL_PREFIX));
    }

    #[test]
    fn stale_child_identity_cannot_match_a_reopened_tab() {
        let owner = BrowserOwner {
            workspace_key: "workspace".to_owned(),
            session_id: "session".to_owned(),
            browser_generation: Some(7),
        };
        let old = BrowserIdentity {
            owner: owner.clone(),
            generation: 11,
        };
        let reopened = BrowserIdentity {
            owner,
            generation: 12,
        };
        assert!(identity_matches(Some(&old), &old));
        assert!(!identity_matches(Some(&reopened), &old));
    }

    #[test]
    fn parse_url_accepts_http_https_and_blank_page() {
        assert_eq!(
            parse_url("https://example.com/a?b=1").unwrap().as_str(),
            "https://example.com/a?b=1"
        );
        // Url 会把无路径的地址规范化为带尾部斜杠。
        assert_eq!(
            parse_url("http://127.0.0.1:8080").unwrap().as_str(),
            "http://127.0.0.1:8080/"
        );
        // 空白页是创建子 WebView 的初始地址，必须放行。
        assert_eq!(parse_url(BLANK_PAGE).unwrap().as_str(), BLANK_PAGE);
    }

    #[test]
    fn parse_url_rejects_non_web_schemes() {
        // 地址栏里的本地路径由前端交给文件预览链路，不经这里加载。
        for raw in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<h1>x</h1>",
            "ftp://example.com",
            "not a url",
        ] {
            assert!(parse_url(raw).is_err(), "{raw} 不应被内置浏览器接受");
        }
    }
}
