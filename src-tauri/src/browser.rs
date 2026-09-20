//! 右侧资源面板的内置浏览器。
//!
//! 每个网页标签对应一个独立原生子 WebView：切换标签只切换显隐，因此各标签的
//! 滚动位置、表单内容和前端路由在切换后保留。
//!
//! 子 WebView 由系统原生层绘制，始终覆盖在主 WebView 之上，主界面的下拉菜单、
//! 弹窗和右键菜单无法盖住它；前端在浮层出现时必须隐藏浏览器。
//!
//! 命令只接受完整 URL：地址补全与本地路径转换由前端纯规则完成。

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, WebviewUrl,
    webview::{PageLoadEvent, WebviewBuilder},
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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserState {
    /// 网页标签标识，与前端标签 id 一致（不含 label 前缀）。
    tab_id: String,
    /// 当前页面地址。
    url: String,
    /// 页面标题；仅标题变化事件携带。
    title: Option<String>,
    /// 事件类别：`started`、`finished` 或 `title`。
    kind: &'static str,
}

/// 生成子 WebView label，并替换 Tauri 不接受的字符。
fn label_for(tab_id: &str) -> String {
    let safe: String = tab_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '/') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{LABEL_PREFIX}{safe}")
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

fn require_webview(app: &AppHandle, tab_id: &str) -> Result<tauri::Webview, String> {
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

fn emit_state(app: &AppHandle, tab_id: &str, url: &str, title: Option<String>, kind: &'static str) {
    let _ = app.emit_to(
        MAIN_WINDOW,
        BROWSER_STATE_EVENT,
        BrowserState {
            tab_id: tab_id.to_string(),
            url: url.to_string(),
            title,
            kind,
        },
    );
}

/// 创建或复用网页标签的子 WebView，并加载目标地址。
///
/// 创建走异步命令：Windows 上在同步命令里创建 WebView 会死锁。
#[tauri::command]
pub async fn browser_open(
    app: AppHandle,
    tab_id: String,
    url: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let target = parse_url(&url)?;

    // 标签重建或地址栏提交时复用既有 WebView，避免丢失页面状态。
    if let Some(webview) = app.get_webview(&label_for(&tab_id)) {
        apply_bounds(&webview, x, y, width, height)?;
        webview
            .navigate(target)
            .map_err(|error| format!("加载浏览器地址失败：{error}"))?;
        return Ok(());
    }

    let window = app
        .get_window(MAIN_WINDOW)
        .ok_or_else(|| "主窗口不存在".to_string())?;

    let blank = Url::parse(BLANK_PAGE).map_err(|error| format!("浏览器初始地址无效：{error}"))?;

    let load_app = app.clone();
    let load_tab = tab_id.clone();
    let title_app = app.clone();
    let title_tab = tab_id.clone();

    let builder = WebviewBuilder::new(label_for(&tab_id), WebviewUrl::External(blank))
        .on_page_load(move |_, payload| {
            let kind = match payload.event() {
                PageLoadEvent::Started => "started",
                PageLoadEvent::Finished => "finished",
            };
            emit_state(&load_app, &load_tab, payload.url().as_str(), None, kind);
        })
        .on_document_title_changed(move |webview, title| {
            let url = webview.url().map(|u| u.to_string()).unwrap_or_default();
            emit_state(&title_app, &title_tab, &url, Some(title), "title");
        });

    let webview = window
        .add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(width, height),
        )
        .map_err(|error| format!("创建内置浏览器失败：{error}"))?;
    webview
        .navigate(target)
        .map_err(|error| format!("加载浏览器地址失败：{error}"))?;
    // 不在这里显示：非激活标签由前端显隐命令控制，避免创建时闪一下。
    Ok(())
}

/// 更新网页标签的显示区域（面板尺寸或分隔条变化时调用）。
#[tauri::command]
pub fn browser_bounds(
    app: AppHandle,
    tab_id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    apply_bounds(&require_webview(&app, &tab_id)?, x, y, width, height)
}

/// 显示网页标签；同一时刻只有当前激活标签可见。
#[tauri::command]
pub fn browser_show(app: AppHandle, tab_id: String) -> Result<(), String> {
    require_webview(&app, &tab_id)?
        .show()
        .map_err(|error| format!("显示内置浏览器失败：{error}"))
}

/// 隐藏网页标签（切换标签、收起面板或浮层出现时调用）。
#[tauri::command]
pub fn browser_hide(app: AppHandle, tab_id: String) -> Result<(), String> {
    require_webview(&app, &tab_id)?
        .hide()
        .map_err(|error| format!("隐藏内置浏览器失败：{error}"))
}

/// 关闭网页标签并释放其 WebView。
#[tauri::command]
pub fn browser_close(app: AppHandle, tab_id: String) -> Result<(), String> {
    let Some(webview) = app.get_webview(&label_for(&tab_id)) else {
        return Ok(());
    };
    webview
        .close()
        .map_err(|error| format!("关闭内置浏览器失败：{error}"))
}

/// 在已有标签内导航到新地址。
#[tauri::command]
pub fn browser_navigate(app: AppHandle, tab_id: String, url: String) -> Result<(), String> {
    let target = parse_url(&url)?;
    require_webview(&app, &tab_id)?
        .navigate(target)
        .map_err(|error| format!("加载浏览器地址失败：{error}"))
}

/// 重新加载当前页面。
#[tauri::command]
pub fn browser_reload(app: AppHandle, tab_id: String) -> Result<(), String> {
    require_webview(&app, &tab_id)?
        .reload()
        .map_err(|error| format!("重新加载页面失败：{error}"))
}

/// 在当前标签的浏览历史中后退或前进。
#[tauri::command]
pub fn browser_history(app: AppHandle, tab_id: String, direction: String) -> Result<(), String> {
    let script = match direction.as_str() {
        "back" => "history.back()",
        "forward" => "history.forward()",
        other => return Err(format!("不支持的历史方向：{other}")),
    };
    require_webview(&app, &tab_id)?
        .eval(script)
        .map_err(|error| format!("切换浏览历史失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_prefixes_tab_id_and_replaces_unsupported_characters() {
        // 前端标签 id 形如 `tab_1758_ab12`；Tauri label 只接受字母数字与 `-_:/.`。
        assert_eq!(label_for("tab_1758_ab12"), "browser-tab_1758_ab12");
        assert_eq!(label_for("a b#c"), "browser-a-b-c");
        // 前缀保证不会与主 WebView 的 label 冲突。
        assert!(label_for("main").starts_with(LABEL_PREFIX));
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
