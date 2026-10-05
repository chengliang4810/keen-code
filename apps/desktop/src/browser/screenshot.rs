//! 原浏览器截图按钮的数据宿主：只捕获当前子 WebView 的可视区域，不截取桌面或其他窗口。

#[cfg(any(windows, test))]
use base64::{Engine, engine::general_purpose::STANDARD};
use tauri::{AppHandle, Webview};

/// 限制跨 IPC 的截图体积，与前端附件准备分离；失败不生成部分图片。
#[cfg(any(windows, test))]
const MAX_PNG_BYTES: usize = 16 * 1024 * 1024;

/// 校验固定 CDP 响应；页面脚本不能指定 CDP 方法或截图目标。
#[cfg(any(windows, test))]
fn screenshot_data(response: &str) -> Result<String, String> {
    if response.len() > MAX_PNG_BYTES.div_ceil(3) * 4 + 1024 {
        return Err("浏览器截图超过 16 MiB 限制".into());
    }
    let value: serde_json::Value =
        serde_json::from_str(response).map_err(|_| "浏览器截图响应无效".to_owned())?;
    let data = value
        .get("data")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "浏览器未返回截图".to_owned())?;
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| "浏览器截图编码无效".to_owned())?;
    if bytes.len() > MAX_PNG_BYTES {
        return Err("浏览器截图超过 16 MiB 限制".into());
    }
    // PNG 签名与首个 IHDR 必须存在，空图和任意字节不能进入原图片附件流程。
    if bytes.len() < 33
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes[8..16] != [0, 0, 0, 13, b'I', b'H', b'D', b'R']
        || bytes[16..20] == [0; 4]
        || bytes[20..24] == [0; 4]
    {
        return Err("浏览器未返回有效 PNG 截图".into());
    }
    Ok(data.to_owned())
}

/// 固定的 Page.captureScreenshot 请求通过 WebView2 回调返回；等待不阻塞窗口线程。
#[tauri::command]
pub async fn browser_capture_screenshot(
    app: AppHandle,
    caller: Webview,
    tab_id: String,
    owner: super::BrowserOwner,
    generation: u64,
) -> Result<String, String> {
    let webview = super::require_webview(&app, &caller, &tab_id, &owner, generation)?;
    #[cfg(windows)]
    {
        let (send, receive) = tokio::sync::oneshot::channel();
        webview.with_webview(move |native| {
            let sender = std::sync::Arc::new(std::sync::Mutex::new(Some(send)));
            let callback_sender = sender.clone();
            let handler = webview2_com::CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |status, response| {
                let result = status.map_err(|error| format!("捕获浏览器截图失败：{error}"))
                    .and_then(|()| screenshot_data(&response));
                if let Some(send) = callback_sender.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    let _ = send.send(result);
                }
                Ok(())
            }));
            // COM 指针仅在 Tauri 指定线程使用，参数是进程内固定字符串。
            let started = unsafe {
                native.controller().CoreWebView2().and_then(|core| core.CallDevToolsProtocolMethod(
                    windows_core::w!("Page.captureScreenshot"),
                    windows_core::w!("{\"format\":\"png\",\"fromSurface\":true,\"captureBeyondViewport\":false}"),
                    &handler,
                ))
            };
            if let Err(error) = started
                && let Some(send) = sender.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = send.send(Err(format!("启动浏览器截图失败：{error}")));
            }
        }).map_err(|e| format!("浏览器截图调度失败：{e}"))?;
        tokio::time::timeout(std::time::Duration::from_secs(10), receive)
            .await
            .map_err(|_| "浏览器截图超时".to_owned())?
            .map_err(|_| "浏览器截图期间标签已关闭".to_owned())?
    }
    #[cfg(not(windows))]
    {
        let _ = webview;
        Err("当前平台尚未对接原生浏览器截图".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_png_response_and_rejects_empty_or_unbounded_data() {
        // 完整的一像素 PNG；只校验宿主返回边界，不模拟 WebView2 渲染。
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6nV8AAAAASUVORK5CYII=";
        assert_eq!(
            screenshot_data(&serde_json::json!({"data":png}).to_string()).unwrap(),
            png
        );
        for response in ["{}", "not-json", "{\"data\":\"%%%\"}", "{\"data\":\"\"}"] {
            assert!(screenshot_data(response).is_err());
        }
        let mut header = STANDARD.decode(png).unwrap();
        header[16..20].fill(0);
        assert!(
            screenshot_data(&serde_json::json!({"data":STANDARD.encode(header)}).to_string())
                .is_err()
        );
        assert!(screenshot_data(&"x".repeat(MAX_PNG_BYTES.div_ceil(3) * 4 + 1025)).is_err());
    }
}
