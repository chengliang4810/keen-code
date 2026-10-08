//! NativeControls 到 GPUI 主窗口的事件投影。
//!
//! NativeHost 的平台回调运行在后台线程，不能直接借用 GPUI。此模块把有界事件
//! 接收器绑定到主窗口，并在 GPUI 前台线程执行窗口、应用和根实体操作。

use std::{sync::Arc, time::Duration};

use gpui::{App, AsyncApp, Entity, PromptButton, PromptLevel, WeakEntity, Window, WindowHandle};
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{
    app_exit,
    native_controls::{
        NativeControlEvent, NativeControlEventReceiver, NativeControls, NativeWindowHandle,
    },
    native_ui::NativeUi,
    native_ui::settings::NativeKeybindingAction,
    tray,
};

/// 从 GPUI 平台窗口提取 NativeControls 使用的真实 Win32 HWND。
///
/// GPUI 自己的 `Window::window_handle()` 是逻辑窗口标识；这里必须显式调用
/// `raw-window-handle` trait，避免把逻辑 ID 错当作 Shell_NotifyIcon 的 HWND。
pub fn native_window_handle(window: &Window) -> Result<NativeWindowHandle, String> {
    #[cfg(not(windows))]
    {
        let _ = window;
        return Ok(NativeWindowHandle::none());
    }

    #[cfg(windows)]
    {
        let handle = HasWindowHandle::window_handle(window)
            .map_err(|error| format!("读取 GPUI 原生窗口句柄失败：{error}"))?;
        match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Ok(NativeWindowHandle::from_raw(handle.hwnd.get())),
            other => Err(format!("GPUI 窗口不是 Win32 句柄：{other:?}")),
        }
    }
}

/// 安装一次 NativeControls 事件消费任务和主窗口关闭拦截器。
///
/// 消费任务在收到 `Quit` 后结束；其余生命周期由 GPUI 前台执行器和事件发送端
/// 共同管理。调用方应在主窗口创建完成后、把根实体返回给 `open_window` 前调用。
pub fn install_native_control_events(
    controls: Arc<NativeControls>,
    receiver: NativeControlEventReceiver,
    ui: Entity<NativeUi>,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    let window_handle = gpui::Window::window_handle(window)
        .downcast::<NativeUi>()
        .ok_or_else(|| "NativeControls 只能安装到 NativeUi 根窗口".to_owned())?;

    let shortcut_controls = Arc::clone(&controls);
    let shortcut_keybindings = controls.keybindings_state();
    let shortcut_ui = ui.downgrade();
    let trace_shortcuts =
        std::env::var_os("KEENCODE_NATIVE_ACCEPTANCE").is_some_and(|value| value == "1");
    let shortcut_subscription = cx.intercept_keystrokes(move |event, _window, cx| {
        let modifiers = event.keystroke.modifiers;
        if trace_shortcuts && modifiers.modified() {
            // 验收只记录动作是否匹配和修饰键数量，不记录用户输入或原始按键文本。
            tracing::info!(
                exit_key_matches =
                    shortcut_keybindings.matches(NativeKeybindingAction::Quit, &event.keystroke),
                modifier_count = modifiers.number_of_modifiers(),
                "Native 组合快捷键到达"
            );
        }

        // 侧栏提示与实际入口共用宿主动作；弱句柄不会形成根实体的订阅引用环。
        if shortcut_keybindings.matches(NativeKeybindingAction::NewChat, &event.keystroke) {
            cx.stop_propagation();
            let _ = shortcut_ui.update(cx, |ui, cx| ui.native_new_chat(cx));
            return;
        }
        if shortcut_keybindings.matches(NativeKeybindingAction::Search, &event.keystroke) {
            cx.stop_propagation();
            let _ = shortcut_ui.update(cx, |ui, cx| ui.native_open_search(cx));
            return;
        }
        if !shortcut_keybindings.matches(NativeKeybindingAction::Quit, &event.keystroke) {
            return;
        }

        // 退出快捷键必须优先于 TextInput 的复制/输入绑定，并复用托盘退出的
        // NativeHost 审批链；不能直接调用 `App::quit` 绕过活动会话确认。
        cx.stop_propagation();
        if let Err(error) = tray::handle_action(shortcut_controls.as_ref(), tray::TrayAction::Quit)
        {
            tracing::error!(%error, "处理退出快捷键失败");
        }
    });
    let chrome_controls = Arc::clone(&controls);
    ui.update(cx, |ui, _| {
        ui.install_native_control_shortcut(shortcut_subscription);
        // 自绘 Close 与系统关闭复用同一入口，保留活动回合确认和关闭到托盘行为。
        ui.install_native_close_handler(move |_, _| {
            if let Err(error) = tray::app_close_window(chrome_controls.as_ref()) {
                tracing::error!(%error, "处理自绘标题栏关闭失败");
            }
        });
    });

    let ui = ui.downgrade();

    let close_controls = Arc::clone(&controls);
    window.on_window_should_close(cx, move |_window, _cx| {
        if close_controls.exit_state().is_approved() {
            return true;
        }
        if let Err(error) = tray::app_close_window(close_controls.as_ref()) {
            tracing::error!(%error, "处理 Native 主窗口关闭请求失败");
        }
        // 退出清理和 close-to-tray 都必须先经过 NativeHost；GPUI 关闭请求不能
        // 绕过退出审批，也不能在隐藏到托盘时销毁窗口。
        false
    });

    #[cfg(windows)]
    install_native_tray_callback(window, Arc::clone(&controls))?;

    let task = cx.spawn(async move |cx| {
        consume_native_control_events(receiver, window_handle, ui, controls, cx).await;
    });
    task.detach();
    Ok(())
}

#[cfg(windows)]
const NATIVE_TRAY_SUBCLASS_ID: usize = 0x4b_43_4f_4e;

#[cfg(windows)]
fn install_native_tray_callback(
    window: &Window,
    controls: Arc<NativeControls>,
) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{SUBCLASSPROC, SetWindowSubclass};

    let window_handle = native_window_handle(window)?;
    let hwnd = window_handle
        .raw()
        .ok_or_else(|| "GPUI 未提供有效的 Win32 HWND".to_owned())?
        as windows_sys::Win32::Foundation::HWND;
    let controls = Box::into_raw(Box::new(controls));
    let callback: SUBCLASSPROC = Some(native_tray_subclass_proc);
    if unsafe { SetWindowSubclass(hwnd, callback, NATIVE_TRAY_SUBCLASS_ID, controls as usize) } == 0
    {
        // SetWindowSubclass 失败时窗口不会替我们释放 ref data。
        unsafe { drop(Box::from_raw(controls)) };
        return Err(format!(
            "安装 Native 托盘窗口消息钩子失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(windows)]
unsafe extern "system" fn native_tray_subclass_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    subclass_id: usize,
    ref_data: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass},
        WindowsAndMessaging::WM_NCDESTROY,
    };

    if ref_data != 0 {
        // SAFETY: SetWindowSubclass 保存的是本函数安装时 Box::into_raw 生成的
        // Arc 指针；直到 WM_NCDESTROY 处理完成前窗口仍持有该指针。
        let controls = unsafe { &*(ref_data as *const Arc<NativeControls>) };
        if message == controls.tray_callback_message() {
            // NOTIFYICON_VERSION_4 把事件放在 lParam 低字，把鼠标坐标放在
            // wParam 的两个有符号 16 位字段；不要把 LPARAM 直接当作事件号。
            let event = (lparam as usize & 0xffff) as u32;
            let x = (wparam as u16 as i16) as i32;
            let y = ((wparam >> 16) as u16 as i16) as i32;
            if let Err(error) = controls.handle_tray_event(event, x, y) {
                tracing::error!(%error, event, "处理 Native 托盘窗口消息失败");
            }
        }
    }

    // 保留 GPUI 的原始 WndProc 链，NativeHost 只消费自己的托盘消息。
    let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    if message == WM_NCDESTROY && ref_data != 0 {
        let callback: windows_sys::Win32::UI::Shell::SUBCLASSPROC = Some(native_tray_subclass_proc);
        unsafe {
            RemoveWindowSubclass(hwnd, callback, subclass_id);
            drop(Box::from_raw(ref_data as *mut Arc<NativeControls>));
        }
    }
    result
}

async fn consume_native_control_events(
    mut receiver: NativeControlEventReceiver,
    window: WindowHandle<NativeUi>,
    ui: WeakEntity<NativeUi>,
    controls: Arc<NativeControls>,
    cx: &mut AsyncApp,
) {
    let mut exit_prompt_open = false;

    while let Some(event) = receiver.recv().await {
        match event {
            NativeControlEvent::ExitRequested(payload) => {
                if exit_prompt_open || controls.exit_state().is_approved() {
                    continue;
                }
                exit_prompt_open = true;
                if request_exit_confirmation(&window, &controls, payload.active_count, cx).await {
                    // confirm_exit() 清理完成后才会由 NativeControls 发布 Quit；这里
                    // 保持消费循环存活，避免确认后提前退出而丢失最后一个事件。
                } else {
                    exit_prompt_open = false;
                }
            }
            NativeControlEvent::Quit => {
                if !controls.exit_state().is_approved() {
                    tracing::error!("收到未获批准的 Native Quit 事件，已忽略");
                    continue;
                }
                if let Err(error) = window.update(cx, |_, _window, app| app.quit()) {
                    tracing::error!(%error, "请求 GPUI 退出失败");
                }
                break;
            }
            event => {
                if apply_native_control_event(event, &ui, &window, cx).is_err() {
                    break;
                }
            }
        }
    }
}

async fn request_exit_confirmation(
    window: &WindowHandle<NativeUi>,
    controls: &Arc<NativeControls>,
    active_count: usize,
    cx: &mut AsyncApp,
) -> bool {
    let detail = format!(
        "仍有 {active_count} 个会话正在运行。确认退出后，KeenCode 会先停止活动工作并保存本地状态。"
    );
    let prompt = window.update(cx, |_ui, window, app| {
        if window.has_active_prompt() {
            return None;
        }
        Some(window.prompt(
            PromptLevel::Warning,
            "确认退出 KeenCode？",
            Some(&detail),
            &[PromptButton::ok("退出"), PromptButton::cancel("取消")],
            app,
        ))
    });

    let receiver = match prompt {
        Ok(Some(receiver)) => receiver,
        Ok(None) => {
            tracing::warn!("退出确认提示已存在，已保留当前提示");
            return false;
        }
        Err(error) => {
            tracing::error!(%error, "显示退出确认提示失败");
            return false;
        }
    };

    match receiver.await {
        Ok(0) => {
            let state = controls.exit_state();
            app_exit::confirm_exit(Arc::clone(controls), Arc::clone(&state));
            while !state.is_approved() {
                // 清理在线程中执行；前台任务只等待状态变化，不能阻塞 GPUI 事件循环。
                cx.background_executor()
                    .timer(Duration::from_millis(25))
                    .await;
            }
            app_exit::run_approved_shutdown(Arc::clone(controls), state);
            true
        }
        Ok(_) => false,
        Err(_) => {
            tracing::warn!("退出确认提示在用户选择前关闭");
            false
        }
    }
}

fn apply_native_control_event(
    event: NativeControlEvent,
    ui: &WeakEntity<NativeUi>,
    window: &WindowHandle<NativeUi>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    match event {
        NativeControlEvent::Show => {
            window.update(cx, |_, window, app| {
                if !window.is_window_active() {
                    window.activate_window();
                }
                app.activate(true);
            })?;
        }
        NativeControlEvent::Hide => {
            window.update(cx, |_, _, app| app.hide())?;
        }
        NativeControlEvent::Activate(ignoring_other_apps) => {
            window.update(cx, |_, window, app| {
                if ignoring_other_apps && !window.is_window_active() {
                    window.activate_window();
                }
                app.activate(ignoring_other_apps);
            })?;
        }
        NativeControlEvent::NewChat => ui.update(cx, |ui, cx| ui.native_new_chat(cx))?,
        NativeControlEvent::OpenSession(session_id) => {
            ui.update(cx, |ui, cx| ui.native_open_session(&session_id, cx))?
        }
        NativeControlEvent::RefreshWorkspace => ui.update(cx, |ui, cx| ui.load_workspace(cx))?,
        NativeControlEvent::UpdateStatus(status) => {
            ui.update(cx, |ui, cx| ui.handle_native_update_status(status, cx))?
        }
        #[cfg(windows)]
        NativeControlEvent::Badge(count) => {
            ui.update(cx, |ui, cx| ui.handle_native_badge(count, cx))?
        }
        NativeControlEvent::ExitRequested(_) | NativeControlEvent::Quit => {
            tracing::error!("退出事件未经过 NativeControl 事件消费器处理");
        }
    }
    Ok(())
}
