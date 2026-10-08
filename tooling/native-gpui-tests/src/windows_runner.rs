//! Windows-only native window driver. No WebView or script injection is used here.

use super::validate_scroll_delta;

use serde_json::{Value, json};
use std::collections::VecDeque;
use std::env;
use std::ffi::c_void;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem::size_of;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{
    CloseHandle, FALSE, FILETIME, GetLastError, HWND, LPARAM, POINT, RECT, TRUE,
};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits, GetWindowDC, HGDIOBJ, ReleaseDC, SRCCOPY,
    SelectObject,
};
use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::StationsAndDesktops::GetThreadDesktop;
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};
use windows_sys::Win32::System::Variant::{
    VARIANT, VT_BOOL, VT_BSTR, VT_I4, VT_I8, VT_LPWSTR, VT_R8, VT_TYPEMASK, VT_UI4, VT_UI8,
    VariantClear,
};
use windows_sys::Win32::UI::Accessibility::{
    UIA_AutomationIdPropertyId, UIA_ClassNamePropertyId, UIA_ControlTypePropertyId,
    UIA_FrameworkIdPropertyId, UIA_NamePropertyId, UIA_ProviderDescriptionPropertyId,
    UiaGetPropertyValue, UiaHasServerSideProvider, UiaNodeFromHandle, UiaNodeRelease,
};
use windows_sys::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    GetAwarenessFromDpiAwarenessContext, GetDpiForWindow, GetThreadDpiAwarenessContext,
    GetWindowDpiAwarenessContext, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
    SetActiveWindow, SetFocus, VIRTUAL_KEY, VK_MENU,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumChildWindows, EnumWindows, GA_ROOT, GetAncestor, GetClassNameW,
    GetClientRect, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed, MSG, PM_NOREMOVE,
    PW_RENDERFULLCONTENT, PeekMessageW, SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER, SetCursorPos,
    SetForegroundWindow, SetWindowPos, ShowWindow, WindowFromPoint,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, WS_EX_NOACTIVATE};
use windows_sys::core::BOOL;

use windows::Win32::Foundation::HWND as WindowsHwnd;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker,
};

// 保持 UIA 证据有界，同时覆盖 GPUI 弹出菜单和权限按钮的实际层级。
const UIA_SUBTREE_MAX_DEPTH: usize = 8;
const UIA_SUBTREE_MAX_NODES: usize = 512;
const UIA_SUBTREE_MAX_TEXT_CHARS: usize = 256;
const UIA_HELPER_TIMEOUT: Duration = Duration::from_secs(5);
const UIA_HELPER_MAX_LINE_BYTES: usize = 2 * 1024 * 1024;
const KEY_EVENT_HOLD_DELAY: Duration = Duration::from_millis(30);
// SendInput 成功只代表入队；GPUI 下一帧才注册新焦点的文本 handler。
// 让真实点击先完成一帧，避免后续 Unicode 字符进入前一字段；持久化断言仍校验实际结果。
const CLICK_FRAME_SETTLE_DELAY: Duration = Duration::from_millis(100);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_RESIZE_CLIENT_DIMENSION: u32 = 16_384;

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub hwnd: Option<String>,
    pub title: Option<String>,
    pub class_name: Option<String>,
}

pub struct NativeWindowDriver {
    hwnd: HWND,
    pid: u32,
}

/// 在首次调用任何窗口坐标或截图 API 前固定 runner 的 DPI 语义。
///
/// 计划坐标和截图必须共享物理 client 坐标系；静默保留 DPI-unaware 会让 Windows
/// 对 `GetWindowRect`、`ClientToScreen` 和 `PrintWindow` 分别虚拟化，导致同一计划
/// 在高 DPI 主机上点击错误位置或生成被裁切证据。
pub fn initialize_process_dpi_awareness() -> Result<(), String> {
    let current = unsafe { GetThreadDpiAwarenessContext() };
    let already_v2 = unsafe {
        AreDpiAwarenessContextsEqual(current, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0
    };
    if !already_v2 {
        let set =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if set == 0 {
            let error = unsafe { GetLastError() };
            return Err(format!(
                "设置 runner PerMonitorV2 DPI awareness 失败：Win32 error {error}"
            ));
        }
    }
    let final_context = unsafe { GetThreadDpiAwarenessContext() };
    let final_v2 = unsafe {
        AreDpiAwarenessContextsEqual(final_context, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0
    };
    if !final_v2 {
        let awareness = unsafe { GetAwarenessFromDpiAwarenessContext(final_context) };
        return Err(format!(
            "runner DPI awareness 设置后仍不是 PerMonitorV2：实际值 {awareness}"
        ));
    }
    Ok(())
}

struct FindWindowState {
    pid: u32,
    hwnd: HWND,
}

struct ChildWindowState {
    nodes: Vec<Value>,
}

pub fn wait_for_window(
    pid: u32,
    timeout: Duration,
    child: &mut Child,
) -> Result<NativeWindowDriver, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(window) = find_window(pid) {
            return Ok(NativeWindowDriver { hwnd: window, pid });
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("被测程序在创建原生窗口前退出：{status}"));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "在 {timeout:?} 内没有找到 PID {pid} 的可见原生窗口"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn find_window(pid: u32) -> Option<HWND> {
    let mut state = FindWindowState {
        pid,
        hwnd: null_mut(),
    };
    unsafe {
        // EnumWindows 回调只写入当前 PID 的顶层可见窗口；不接受其它进程窗口。
        EnumWindows(Some(find_window_callback), &mut state as *mut _ as LPARAM);
    }
    (!state.hwnd.is_null()).then_some(state.hwnd)
}

unsafe extern "system" fn find_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = unsafe { &mut *(lparam as *mut FindWindowState) };
    if unsafe { IsWindowVisible(hwnd) } == 0 {
        return TRUE;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == state.pid {
        state.hwnd = hwnd;
        return FALSE;
    }
    TRUE
}

impl NativeWindowDriver {
    pub fn info(&self) -> WindowInfo {
        WindowInfo {
            hwnd: (!self.hwnd.is_null()).then(|| format!("0x{:x}", self.hwnd as usize)),
            title: window_text(self.hwnd),
            class_name: class_name(self.hwnd),
        }
    }

    pub fn focus(&self) -> Result<(), String> {
        // Windows 的前台限制看的是调用 SetForegroundWindow 的线程输入队列。
        // 之前把“前台线程”附着到目标线程，runner 自身仍不在前台队列中，
        // 因而即使目标 HWND 有效且可见，SetForegroundWindow 仍会返回失败。
        let current_thread = unsafe { GetCurrentThreadId() };
        let target_thread = unsafe { GetWindowThreadProcessId(self.hwnd, null_mut()) };
        let foreground = unsafe { GetForegroundWindow() };
        let mut foreground_pid = 0u32;
        let foreground_thread = if foreground.is_null() {
            current_thread
        } else {
            unsafe { GetWindowThreadProcessId(foreground, &mut foreground_pid) }
        };
        let current_desktop = unsafe { GetThreadDesktop(current_thread) };
        let target_desktop = unsafe { GetThreadDesktop(target_thread) };
        let foreground_desktop = unsafe { GetThreadDesktop(foreground_thread) };
        // AttachThreadInput 要求调用线程已经有消息队列；runner 本身是控制台
        // 进程，先用 PeekMessageW 创建队列，不发送或伪造任何业务输入消息。
        let mut message = MSG::default();
        unsafe {
            PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
        }
        let mut attached_foreground = false;
        let mut foreground_attach_direction = "none";
        let mut foreground_attach_error = 0;
        let mut foreground_reverse_attach_error = 0;
        let mut attached_target_for_activation = false;
        let mut target_attach_error = 0;
        if foreground_thread != 0
            && foreground_thread != current_thread
            && foreground_thread != target_thread
        {
            attached_foreground =
                unsafe { AttachThreadInput(current_thread, foreground_thread, TRUE) != 0 };
            if attached_foreground {
                foreground_attach_direction = "current_to_foreground";
            } else {
                foreground_attach_error = unsafe { GetLastError() };
                // 某些 Windows 桌面线程只接受由前台线程发起的队列合并；两种调用
                // 方向语义相同，但分别记录方向，确保成功后按原方向解除附着。
                attached_foreground =
                    unsafe { AttachThreadInput(foreground_thread, current_thread, TRUE) != 0 };
                if attached_foreground {
                    foreground_attach_direction = "foreground_to_current";
                } else {
                    foreground_reverse_attach_error = unsafe { GetLastError() };
                }
            }
        }
        let mut last_error;
        let mut last_foreground;
        let mut foreground_set;
        let mut alt_activation_attempted = false;
        let mut alt_activation_error = None;
        let deadline = Instant::now() + Duration::from_millis(500);
        unsafe {
            loop {
                if !attached_foreground
                    && foreground_thread != 0
                    && foreground_thread != current_thread
                    && foreground_thread != target_thread
                {
                    attached_foreground =
                        AttachThreadInput(current_thread, foreground_thread, TRUE) != 0;
                    if attached_foreground {
                        foreground_attach_direction = "current_to_foreground";
                    } else {
                        foreground_attach_error = GetLastError();
                        attached_foreground =
                            AttachThreadInput(foreground_thread, current_thread, TRUE) != 0;
                        if attached_foreground {
                            foreground_attach_direction = "foreground_to_current";
                        } else {
                            foreground_reverse_attach_error = GetLastError();
                        }
                    }
                }
                if !attached_target_for_activation
                    && target_thread != 0
                    && target_thread != current_thread
                {
                    attached_target_for_activation =
                        AttachThreadInput(current_thread, target_thread, TRUE) != 0;
                    if !attached_target_for_activation {
                        target_attach_error = GetLastError();
                    }
                }
                if !alt_activation_attempted && foreground != self.hwnd {
                    // 前台线程属于受保护的桌面窗口时，AttachThreadInput 可能返回
                    // ERROR_ACCESS_DENIED。一次真实 Alt 按键序列让 Windows 记录当前
                    // runner 为最近输入来源，再重试正式的 SetForegroundWindow。
                    alt_activation_attempted = true;
                    let alt_inputs = [
                        keyboard_input(VK_MENU, 0),
                        keyboard_input(VK_MENU, KEYEVENTF_KEYUP),
                    ];
                    if let Err(error) = send_inputs(&alt_inputs) {
                        alt_activation_error = Some(error);
                    }
                }
                ShowWindow(self.hwnd, SW_RESTORE);
                BringWindowToTop(self.hwnd);
                foreground_set = SetForegroundWindow(self.hwnd) != 0;
                last_foreground = GetForegroundWindow();
                if foreground_set && last_foreground == self.hwnd {
                    if attached_foreground {
                        if foreground_attach_direction == "foreground_to_current" {
                            AttachThreadInput(foreground_thread, current_thread, FALSE);
                        } else {
                            AttachThreadInput(current_thread, foreground_thread, FALSE);
                        }
                        attached_foreground = false;
                    }
                    if attached_target_for_activation {
                        AttachThreadInput(current_thread, target_thread, FALSE);
                        attached_target_for_activation = false;
                    }
                    // SetActiveWindow/SetFocus 只能作用于调用线程所属输入队列；
                    // 目标线程完成前台切换后再短暂合并 runner 与目标队列。
                    let attached_target = target_thread != 0
                        && target_thread != current_thread
                        && AttachThreadInput(current_thread, target_thread, TRUE) != 0;
                    SetActiveWindow(self.hwnd);
                    SetFocus(self.hwnd);
                    if attached_target {
                        AttachThreadInput(current_thread, target_thread, FALSE);
                    }
                    last_foreground = GetForegroundWindow();
                    if last_foreground == self.hwnd {
                        return Ok(());
                    }
                }
                if !foreground_set {
                    last_error = GetLastError();
                } else {
                    last_error = 0;
                }
                if Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
        if attached_foreground {
            unsafe {
                if foreground_attach_direction == "foreground_to_current" {
                    AttachThreadInput(foreground_thread, current_thread, FALSE);
                } else {
                    AttachThreadInput(current_thread, foreground_thread, FALSE);
                }
            }
        }
        if attached_target_for_activation {
            unsafe {
                AttachThreadInput(current_thread, target_thread, FALSE);
            }
        }
        let mouse_activation_error;
        if let Err(error) = self.activate_with_titlebar_click() {
            mouse_activation_error = Some(error);
        } else {
            let mouse_deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < mouse_deadline {
                if unsafe { GetForegroundWindow() } == self.hwnd {
                    let attached_target = target_thread != 0
                        && target_thread != current_thread
                        && unsafe { AttachThreadInput(current_thread, target_thread, TRUE) != 0 };
                    unsafe {
                        SetActiveWindow(self.hwnd);
                        SetFocus(self.hwnd);
                    }
                    if attached_target {
                        unsafe {
                            AttachThreadInput(current_thread, target_thread, FALSE);
                        }
                    }
                    if unsafe { GetForegroundWindow() } == self.hwnd {
                        return Ok(());
                    }
                }
                thread::sleep(Duration::from_millis(25));
            }
            mouse_activation_error = Some("标题栏真实点击后仍未成为前台窗口".to_owned());
        }
        let foreground = unsafe { GetForegroundWindow() };
        let extended_style = unsafe { GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32 };
        Err(format!(
            "SetForegroundWindow 失败 (Win32 error {last_error}, foreground_set={foreground_set}, hwnd=0x{:x}, foreground=0x{:x}, last_foreground=0x{:x}, current_thread={current_thread}, target_thread={target_thread}, foreground_thread={foreground_thread}, foreground_pid={foreground_pid}, desktops_equal={}, current_desktop=0x{:x}, target_desktop=0x{:x}, foreground_desktop=0x{:x}, attached_foreground={attached_foreground}, foreground_attach_direction={foreground_attach_direction}, foreground_attach_error={foreground_attach_error}, foreground_reverse_attach_error={foreground_reverse_attach_error}, attached_target_for_activation={attached_target_for_activation}, target_attach_error={target_attach_error}, alt_activation_attempted={alt_activation_attempted}, alt_activation_error={:?}, mouse_activation_error={:?}, valid={}, visible={}, no_activate={})",
            self.hwnd as usize,
            foreground as usize,
            last_foreground as usize,
            current_desktop == target_desktop && current_desktop == foreground_desktop,
            current_desktop as usize,
            target_desktop as usize,
            foreground_desktop as usize,
            alt_activation_error,
            mouse_activation_error,
            unsafe { IsWindow(self.hwnd) != 0 },
            unsafe { IsWindowVisible(self.hwnd) != 0 },
            extended_style & WS_EX_NOACTIVATE != 0,
        ))
    }

    fn activate_with_titlebar_click(&self) -> Result<(), String> {
        let mut client = RECT::default();
        unsafe {
            if GetClientRect(self.hwnd, &mut client) == 0 {
                return Err("获取目标 client 矩形失败，拒绝标题栏点击".to_owned());
            }
        }
        let width = client.right - client.left;
        let height = client.bottom - client.top;
        if width <= 0 || height <= 0 {
            return Err("目标 client 矩形为空，拒绝标题栏点击".to_owned());
        }
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 {
            return Err("获取目标窗口 DPI 失败，拒绝标题栏点击".to_owned());
        }
        // 目标窗口顶栏位于 client 内；用固定逻辑坐标换算物理像素，避开左侧控件。
        let mut point = POINT {
            x: client.left + width / 2,
            y: client.top + ((12u32 * dpi + 48) / 96) as i32,
        };
        if point.y >= client.bottom {
            return Err("标题栏校验点超出目标 client，拒绝点击".to_owned());
        }
        unsafe {
            if ClientToScreen(self.hwnd, &mut point) == 0 {
                return Err("标题栏校验点转换到屏幕失败，拒绝点击".to_owned());
            }
            let hit = WindowFromPoint(point);
            let root = if hit.is_null() {
                null_mut()
            } else {
                GetAncestor(hit, GA_ROOT)
            };
            if hit.is_null() || root != self.hwnd || IsWindowVisible(hit) == 0 {
                return Err(format!(
                    "标题栏校验点未命中目标 HWND (point={},{} hit=0x{:x} root=0x{:x})",
                    point.x, point.y, hit as usize, root as usize
                ));
            }
            if SetCursorPos(point.x, point.y) == 0 {
                return Err("标题栏点击 SetCursorPos 失败".to_owned());
            }
        }
        send_inputs(&left_click_inputs())
    }

    pub fn click(&self, x: i32, y: i32) -> Result<(), String> {
        self.focus()?;
        let mut point = POINT { x, y };
        unsafe {
            if ClientToScreen(self.hwnd, &mut point) == 0 {
                return Err("ClientToScreen 失败".to_owned());
            }
            if SetCursorPos(point.x, point.y) == 0 {
                return Err("SetCursorPos 失败".to_owned());
            }
        }
        send_inputs(&left_click_inputs())?;
        thread::sleep(CLICK_FRAME_SETTLE_DELAY);
        Ok(())
    }

    /// 将 client 坐标转换为屏幕坐标后注入真实 Win32 垂直滚轮事件。
    ///
    /// GPUI 的滚轮处理依赖指针所在的真实控件；因此这里必须把光标移动到
    /// 目标 HWND 的 client 点，再发送带符号的 `MOUSEEVENTF_WHEEL` 输入。
    pub fn scroll(&self, x: i32, y: i32, delta_y: i32) -> Result<(), String> {
        validate_scroll_delta(delta_y)?;
        let (client_width, client_height) = self.client_size()?;
        validate_scroll_point(x, y, client_width, client_height)?;
        self.focus()?;
        let mut point = POINT { x, y };
        unsafe {
            if ClientToScreen(self.hwnd, &mut point) == 0 {
                return Err("scroll ClientToScreen 失败".to_owned());
            }
            if SetCursorPos(point.x, point.y) == 0 {
                return Err("scroll SetCursorPos 失败".to_owned());
            }
        }
        send_inputs(&[mouse_wheel_input(delta_y)])
    }

    pub fn client_size(&self) -> Result<(u32, u32), String> {
        let mut client = RECT::default();
        unsafe {
            if GetClientRect(self.hwnd, &mut client) == 0 {
                return Err("获取目标 client 矩形失败".to_owned());
            }
        }
        let width = client.right.saturating_sub(client.left);
        let height = client.bottom.saturating_sub(client.top);
        if width <= 0 || height <= 0 {
            return Err(format!("目标 client 矩形为空：{}x{}", width, height));
        }
        Ok((width as u32, height as u32))
    }

    /// 将 UI Automation 提供的屏幕坐标转换为目标 HWND 的 client 坐标。
    pub fn screen_to_client(&self, x: i32, y: i32) -> Result<(i32, i32), String> {
        let mut point = POINT { x, y };
        unsafe {
            if ScreenToClient(self.hwnd, &mut point) == 0 {
                return Err("ScreenToClient 失败".to_owned());
            }
        }
        Ok((point.x, point.y))
    }

    pub fn type_text(&self, text: &str) -> Result<(), String> {
        self.focus()?;
        for unit in text.encode_utf16() {
            let inputs = [
                keyboard_input(unit, KEYEVENTF_UNICODE),
                keyboard_input(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ];
            send_inputs(&inputs)?;
        }
        Ok(())
    }

    pub fn key(&self, key: &str, modifiers: &[String]) -> Result<(), String> {
        self.focus()?;
        let virtual_key = key_to_vk(key)?;
        let modifier_keys = modifiers
            .iter()
            .map(|modifier| modifier_to_vk(modifier))
            .collect::<Result<Vec<_>, _>>()?;
        // GPUI 可能在一次 SendInput 批处理中错过修饰键状态；分阶段按下、保持、释放，
        // 让 Ctrl+Q 等组合键在真实窗口消息循环中至少可见 30ms。
        for modifier in &modifier_keys {
            send_inputs(&[keyboard_input(*modifier, 0)])?;
            thread::sleep(KEY_EVENT_HOLD_DELAY);
        }
        send_inputs(&[keyboard_input(virtual_key, 0)])?;
        thread::sleep(KEY_EVENT_HOLD_DELAY);
        send_inputs(&[keyboard_input(virtual_key, KEYEVENTF_KEYUP)])?;
        thread::sleep(KEY_EVENT_HOLD_DELAY);
        for modifier in modifier_keys.iter().rev() {
            send_inputs(&[keyboard_input(*modifier, KEYEVENTF_KEYUP)])?;
            thread::sleep(KEY_EVENT_HOLD_DELAY);
        }
        Ok(())
    }

    pub fn resize_client(&self, width: u32, height: u32) -> Result<Value, String> {
        if width == 0
            || height == 0
            || width > MAX_RESIZE_CLIENT_DIMENSION
            || height > MAX_RESIZE_CLIENT_DIMENSION
        {
            return Err(format!(
                "resize_client 的 client 尺寸必须在 1..{} 范围内：{}x{}",
                MAX_RESIZE_CLIENT_DIMENSION, width, height
            ));
        }
        if unsafe { IsWindow(self.hwnd) } == 0 {
            return Err("resize_client 目标 HWND 无效".to_owned());
        }

        let (window_rect, client_rect, restored) = unsafe {
            let minimized = IsIconic(self.hwnd) != 0;
            let maximized = IsZoomed(self.hwnd) != 0;
            if minimized || maximized {
                // 最大化/最小化时 client rect 不能作为普通窗口的非 client 边界；
                // SW_RESTORE 只恢复窗口状态，后续 SetWindowPos 仍禁止激活和改 Z 序。
                ShowWindow(self.hwnd, SW_RESTORE);
            }
            let mut window_rect = RECT::default();
            if GetWindowRect(self.hwnd, &mut window_rect) == 0 {
                return Err("resize_client 获取窗口矩形失败".to_owned());
            }
            let mut client_rect = RECT::default();
            if GetClientRect(self.hwnd, &mut client_rect) == 0 {
                return Err("resize_client 获取 client 矩形失败".to_owned());
            }
            (window_rect, client_rect, minimized || maximized)
        };

        let window_width = i64::from(window_rect.right) - i64::from(window_rect.left);
        let window_height = i64::from(window_rect.bottom) - i64::from(window_rect.top);
        let client_width = i64::from(client_rect.right) - i64::from(client_rect.left);
        let client_height = i64::from(client_rect.bottom) - i64::from(client_rect.top);
        if window_width <= 0
            || window_height <= 0
            || client_width <= 0
            || client_height <= 0
            || window_width < client_width
            || window_height < client_height
        {
            return Err(format!(
                "resize_client 无法计算有效 non-client 边界：window={}x{} client={}x{}",
                window_width, window_height, client_width, client_height
            ));
        }
        let non_client_width = window_width - client_width;
        let non_client_height = window_height - client_height;
        let outer_width = i64::from(width) + non_client_width;
        let outer_height = i64::from(height) + non_client_height;
        let outer_width = i32::try_from(outer_width)
            .map_err(|_| format!("resize_client 计算出的窗口宽度超出 Win32 范围：{outer_width}"))?;
        let outer_height = i32::try_from(outer_height).map_err(|_| {
            format!("resize_client 计算出的窗口高度超出 Win32 范围：{outer_height}")
        })?;

        let set = unsafe {
            SetWindowPos(
                self.hwnd,
                null_mut(),
                window_rect.left,
                window_rect.top,
                outer_width,
                outer_height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        };
        if set == 0 {
            let error = unsafe { GetLastError() };
            return Err(format!(
                "resize_client SetWindowPos 失败：Win32 error {error}"
            ));
        }
        Ok(json!({
            "requestedClientWidth": width,
            "requestedClientHeight": height,
            "nonClientWidth": non_client_width,
            "nonClientHeight": non_client_height,
            "windowLeft": window_rect.left,
            "windowTop": window_rect.top,
            "restored": restored,
            "setWindowPos": true,
            "note": "仅报告 SetWindowPos 已成功；计划必须在后续 wait 后用 assert_window 验证实际 client 尺寸"
        }))
    }

    pub fn close_window(&self) -> Result<(), String> {
        if unsafe { IsWindow(self.hwnd) } == 0 {
            return Ok(());
        }
        // Ctrl+Q 经过生产 NativeControls 的退出确认链；Alt+F4 可能按 closeToTray
        // 隐藏到托盘，无法证明被测进程真的退出。
        self.key("q", &["ctrl".to_owned()])
    }

    pub fn screenshot(&self, path: &Path) -> Result<Value, String> {
        let mut rect = RECT::default();
        unsafe {
            if GetWindowRect(self.hwnd, &mut rect) == 0 {
                return Err("GetWindowRect 失败".to_owned());
            }
        }
        let width = (rect.right - rect.left).max(1);
        let height = (rect.bottom - rect.top).max(1);
        let hdc = unsafe { GetWindowDC(self.hwnd) };
        if hdc.is_null() {
            return Err("GetWindowDC 失败".to_owned());
        }
        let memdc = unsafe { CreateCompatibleDC(hdc) };
        let bitmap = unsafe { CreateCompatibleBitmap(hdc, width, height) };
        if memdc.is_null() || bitmap.is_null() {
            unsafe {
                if !memdc.is_null() {
                    DeleteDC(memdc);
                }
                if !bitmap.is_null() {
                    DeleteObject(bitmap as HGDIOBJ);
                }
                ReleaseDC(self.hwnd, hdc);
            }
            return Err("创建截图 GDI 对象失败".to_owned());
        }
        let old = unsafe { SelectObject(memdc, bitmap as HGDIOBJ) };
        let printed = unsafe { PrintWindow(self.hwnd, memdc, PW_RENDERFULLCONTENT) != 0 };
        if !printed {
            unsafe {
                BitBlt(memdc, 0, 0, width, height, hdc, 0, 0, SRCCOPY);
            }
        }
        let mut bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        let copied = unsafe {
            GetDIBits(
                memdc,
                bitmap,
                0,
                height as u32,
                pixels.as_mut_ptr() as *mut c_void,
                &mut bitmap_info,
                DIB_RGB_COLORS,
            )
        };
        unsafe {
            SelectObject(memdc, old);
            DeleteObject(bitmap as HGDIOBJ);
            DeleteDC(memdc);
            ReleaseDC(self.hwnd, hdc);
        }
        if copied == 0 {
            return Err("GetDIBits 失败".to_owned());
        }
        write_bmp(path, width as u32, height as u32, &pixels)?;
        Ok(json!({
            "path": path.display().to_string(),
            "format": "BMP",
            "width": width,
            "height": height,
            "printWindow": printed,
            "bytes": pixels.len() + 54,
        }))
    }

    pub fn accessibility_tree(&self) -> Result<Value, String> {
        accessibility_tree_via_helper(self)
    }

    fn accessibility_tree_direct(
        &self,
        progress: &mut impl FnMut(&str, Option<&Value>) -> Result<(), String>,
    ) -> Result<Value, String> {
        let _com = ComGuard::new()?;
        progress("com-initialize:done", None)?;
        progress("uia-has-server-side-provider:start", None)?;
        let has_server_side_provider = unsafe { UiaHasServerSideProvider(self.hwnd) != 0 };
        let provider_value = json!(has_server_side_provider);
        progress("uia-has-server-side-provider:done", Some(&provider_value))?;
        progress("uia-node-from-handle:start", None)?;
        let mut node = null_mut();
        let root_hr = unsafe { UiaNodeFromHandle(self.hwnd, &mut node) };
        let root_hresult = json!(format_hresult(root_hr));
        progress("uia-node-from-handle:done", Some(&root_hresult))?;
        let mut properties = std::collections::BTreeMap::new();
        if !node.is_null() {
            for (name, property_id) in [
                ("name", UIA_NamePropertyId),
                ("automationId", UIA_AutomationIdPropertyId),
                ("className", UIA_ClassNamePropertyId),
                ("controlType", UIA_ControlTypePropertyId),
                ("frameworkId", UIA_FrameworkIdPropertyId),
                ("providerDescription", UIA_ProviderDescriptionPropertyId),
            ] {
                let stage = format!("uia-get-property:{name}:start");
                progress(&stage, None)?;
                let mut variant = VARIANT::default();
                let property_hr = unsafe { UiaGetPropertyValue(node, property_id, &mut variant) };
                properties.insert(
                    name.to_owned(),
                    json!({
                        "hresult": format_hresult(property_hr),
                        "value": variant_value(&variant),
                    }),
                );
                unsafe {
                    VariantClear(&mut variant);
                }
                let stage = format!("uia-get-property:{name}:done");
                progress(&stage, None)?;
            }
        }
        let mut child_state = ChildWindowState { nodes: Vec::new() };
        unsafe {
            EnumChildWindows(
                self.hwnd,
                Some(child_window_callback),
                &mut child_state as *mut _ as LPARAM,
            );
        }
        let root = json!({
            "schema": "keencode/accesskit-native-tree",
            "transport": "Windows UI Automation (AccessKit provider surface)",
            "rootHwnd": format!("0x{:x}", self.hwnd as usize),
            "processId": self.pid,
            "uiaHasServerSideProvider": has_server_side_provider,
            "provider": {
                "status": if has_server_side_provider { "available" } else { "unavailable" },
                "source": "UiaHasServerSideProvider",
            },
            "uiaNodeFromHandle": format_hresult(root_hr),
            "rootProperties": properties,
            "subtree": {
                "status": "pending",
                "reason": "UIA helper is still collecting the bounded subtree",
            },
            "childWindows": child_state.nodes.clone(),
            "boundary": "subtree.nodes 是通过标准 IUIAutomation TreeWalker 读取的真实控件子树；childWindows 仅是 HWND 诊断，不替代 UIA 控件树",
        });
        progress("root-ready", Some(&root))?;
        let subtree = if !has_server_side_provider {
            progress("uia-treewalker:skipped-provider-unavailable", None)?;
            json!({
                "status": "unavailable",
                "hresult": format_hresult(root_hr),
                "maxDepth": UIA_SUBTREE_MAX_DEPTH,
                "nodeLimit": UIA_SUBTREE_MAX_NODES,
                "textLimitChars": UIA_SUBTREE_MAX_TEXT_CHARS,
                "reason": "UiaHasServerSideProvider returned false",
                "source": "Windows UI Automation IUIAutomation",
            })
        } else {
            progress("uia-element-from-handle:start", None)?;
            let standard_result =
                unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }.and_then(
                    |automation: IUIAutomation| unsafe {
                        automation
                            .ElementFromHandle(WindowsHwnd(self.hwnd))
                            .map(|element| (automation, element))
                    },
                );
            match standard_result {
                Err(error) => {
                    let error_value = json!(format_hresult(error.code().0));
                    progress("uia-element-from-handle:failed", Some(&error_value))?;
                    json!({
                        "status": "unavailable",
                        "hresult": format_hresult(error.code().0),
                        "maxDepth": UIA_SUBTREE_MAX_DEPTH,
                        "nodeLimit": UIA_SUBTREE_MAX_NODES,
                        "textLimitChars": UIA_SUBTREE_MAX_TEXT_CHARS,
                        "reason": "IUIAutomation::ElementFromHandle failed",
                        "source": "Windows UI Automation IUIAutomation::ElementFromHandle",
                    })
                }
                Ok((automation, element)) => {
                    progress("uia-element-from-handle:done", None)?;
                    progress("uia-treewalker:start", None)?;
                    let walker_result = unsafe { automation.ControlViewWalker() };
                    match walker_result {
                        Err(error) => {
                            let error_value = json!(format_hresult(error.code().0));
                            progress("uia-treewalker:failed", Some(&error_value))?;
                            json!({
                                "status": "unavailable",
                                "hresult": format_hresult(error.code().0),
                                "maxDepth": UIA_SUBTREE_MAX_DEPTH,
                                "nodeLimit": UIA_SUBTREE_MAX_NODES,
                                "textLimitChars": UIA_SUBTREE_MAX_TEXT_CHARS,
                                "reason": "IUIAutomation::ControlViewWalker failed",
                                "source": "Windows UI Automation IUIAutomation::ControlViewWalker",
                            })
                        }
                        Ok(walker) => {
                            progress("uia-treewalker:done", None)?;
                            read_uia_subtree(element, walker)
                        }
                    }
                }
            }
        };
        if !node.is_null() {
            unsafe {
                UiaNodeRelease(node);
            }
        }
        let result = json!({
            "schema": "keencode/accesskit-native-tree",
            "transport": "Windows UI Automation (AccessKit provider surface)",
            "rootHwnd": format!("0x{:x}", self.hwnd as usize),
            "processId": self.pid,
            "uiaHasServerSideProvider": has_server_side_provider,
            "provider": {
                "status": if has_server_side_provider { "available" } else { "unavailable" },
                "source": "UiaHasServerSideProvider",
            },
            "uiaNodeFromHandle": format_hresult(root_hr),
            "rootProperties": root["rootProperties"].clone(),
            "subtree": subtree,
            "childWindows": child_state.nodes,
            "boundary": "subtree.nodes 是通过标准 IUIAutomation TreeWalker 读取的真实控件子树；childWindows 仅是 HWND 诊断，不替代 UIA 控件树",
        });
        progress("complete", Some(&result))?;
        Ok(result)
    }

    pub fn metrics(&self) -> Result<Value, String> {
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
                FALSE,
                self.pid,
            )
        };
        if handle.is_null() {
            return Err("OpenProcess 查询指标失败".to_owned());
        }
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        let memory_ok = unsafe {
            GetProcessMemoryInfo(
                handle,
                &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
                size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ) != 0
        };
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let times_ok = unsafe {
            GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) != 0
        };
        unsafe {
            CloseHandle(handle);
        }
        let window = window_geometry(self.hwnd);
        // 用 Unix 毫秒记录每次独立采样，便于按相邻样本的时间差计算 CPU 占用率。
        let sampled_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        let logical_cpu_count = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);
        Ok(json!({
            "pid": self.pid,
            "sampledAtMs": sampled_at_ms,
            "logicalCpuCount": logical_cpu_count,
            "privateBytes": memory_ok.then_some(counters.PrivateUsage),
            "workingSetBytes": memory_ok.then_some(counters.WorkingSetSize),
            "pagefileBytes": memory_ok.then_some(counters.PagefileUsage),
            "userCpuMs": times_ok.then_some(filetime_ms(user)),
            "kernelCpuMs": times_ok.then_some(filetime_ms(kernel)),
            "window": window,
            "coordinateInput": {
                "space": "client coordinates passed to ClientToScreen",
                "injection": "SetCursorPos followed by SendInput left click",
                "runnerDpiAwareness": dpi_awareness_name(unsafe {
                    GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext())
                }),
            },
            "gpu": {
                "status": "pending",
                "source": "Windows ETW/GPU Engine provider not enabled by this offline-safe runner",
            },
        }))
    }
}

pub fn maybe_run_uia_helper() -> Option<Result<(), String>> {
    let mut args = env::args().skip(1);
    if args.next().as_deref() != Some("--uia-helper") {
        return None;
    }
    // helper 是独立进程，必须在 UIA 调用前设置 DPI；不能继承主 runner 的已初始化状态。
    // 否则 200% 缩放时 UIA 返回虚拟化矩形，SendInput 会点击到相邻甚至其他页面。
    if let Err(error) = initialize_process_dpi_awareness() {
        let _ = write_uia_helper_message("error", Some(&error), None);
        return Some(Err(error));
    }
    let hwnd_text = match next_uia_helper_arg(&mut args, "--hwnd") {
        Ok(value) => value,
        Err(error) => return Some(Err(error)),
    };
    let pid_text = match next_uia_helper_arg(&mut args, "--pid") {
        Ok(value) => value,
        Err(error) => return Some(Err(error)),
    };
    let hwnd_value = hwnd_text
        .strip_prefix("0x")
        .or_else(|| hwnd_text.strip_prefix("0X"))
        .ok_or_else(|| "UIA helper 的 --hwnd 必须是 0x 开头的十六进制句柄".to_owned());
    let hwnd_value = match hwnd_value.and_then(|value| {
        usize::from_str_radix(value, 16).map_err(|_| "UIA helper 的 --hwnd 不是有效句柄".to_owned())
    }) {
        Ok(value) if value != 0 => value,
        Ok(_) => return Some(Err("UIA helper 的 --hwnd 不能为 NULL".to_owned())),
        Err(error) => return Some(Err(error)),
    };
    let pid = match pid_text.parse::<u32>() {
        Ok(value) if value != 0 => value,
        _ => return Some(Err("UIA helper 的 --pid 不是有效进程 ID".to_owned())),
    };
    let driver = NativeWindowDriver {
        hwnd: hwnd_value as HWND,
        pid,
    };
    let mut progress =
        |stage: &str, tree: Option<&Value>| write_uia_helper_message("stage", Some(stage), tree);
    match driver.accessibility_tree_direct(&mut progress) {
        Ok(_) => Some(Ok(())),
        Err(error) => {
            let _ = write_uia_helper_message("error", Some(&error), None);
            Some(Err(error))
        }
    }
}

fn next_uia_helper_arg(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<String, String> {
    args.next()
        .filter(|value| value.as_str() == flag)
        .and_then(|_| args.next())
        .ok_or_else(|| format!("UIA helper 缺少 {flag} 参数"))
}

fn write_uia_helper_message(
    kind: &str,
    stage: Option<&str>,
    tree: Option<&Value>,
) -> Result<(), String> {
    let mut message = serde_json::Map::new();
    message.insert("kind".to_owned(), Value::String(kind.to_owned()));
    if let Some(stage) = stage {
        message.insert("stage".to_owned(), Value::String(stage.to_owned()));
    }
    if let Some(tree) = tree {
        message.insert("tree".to_owned(), tree.clone());
    }
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &Value::Object(message))
        .map_err(|error| format!("写入 UIA helper 消息失败：{error}"))?;
    stdout
        .write_all(b"\n")
        .map_err(|error| format!("写入 UIA helper 消息换行失败：{error}"))?;
    stdout
        .flush()
        .map_err(|error| format!("刷新 UIA helper 消息失败：{error}"))
}

fn accessibility_tree_via_helper(driver: &NativeWindowDriver) -> Result<Value, String> {
    let mut child = match spawn_uia_helper(driver) {
        Ok(child) => child,
        Err(error) => {
            return Ok(accessibility_tree_fallback(
                driver,
                None,
                "unavailable",
                "spawn:start",
                Some(error),
            ));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(accessibility_tree_fallback(
                driver,
                None,
                "unavailable",
                "spawn:stdout-missing",
                Some("UIA helper 未提供 stdout 管道".to_owned()),
            ));
        }
    };
    let (messages, reader) = spawn_uia_stdout_reader(stdout);
    let deadline = Instant::now() + UIA_HELPER_TIMEOUT;
    let mut last_stage = "spawned".to_owned();
    let mut partial_tree = None;
    let mut completed_tree = None;
    let mut helper_error = None;
    let mut timed_out = false;
    let mut child_finished = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                child_finished = true;
                break;
            }
            Ok(None) => {}
            Err(error) => {
                helper_error = Some(format!("读取 UIA helper 状态失败：{error}"));
                break;
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            timed_out = true;
            break;
        }
        match messages.recv_timeout(remaining.min(Duration::from_millis(50))) {
            Ok(line) => {
                if apply_uia_helper_message(
                    &line,
                    &mut last_stage,
                    &mut partial_tree,
                    &mut completed_tree,
                    &mut helper_error,
                ) {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                helper_error = Some("UIA helper 输出管道提前关闭".to_owned());
                break;
            }
        }
    }
    if !child_finished || timed_out || completed_tree.is_some() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let _ = reader.join();
    while let Ok(line) = messages.try_recv() {
        let _ = apply_uia_helper_message(
            &line,
            &mut last_stage,
            &mut partial_tree,
            &mut completed_tree,
            &mut helper_error,
        );
    }
    if let Some(tree) = completed_tree {
        return Ok(annotate_uia_helper_result(
            tree,
            "completed",
            &last_stage,
            None,
        ));
    }
    Ok(accessibility_tree_fallback(
        driver,
        partial_tree,
        if timed_out { "timeout" } else { "unavailable" },
        &last_stage,
        helper_error,
    ))
}

fn spawn_uia_helper(driver: &NativeWindowDriver) -> Result<Child, String> {
    let executable =
        env::current_exe().map_err(|error| format!("定位 UIA helper 可执行文件失败：{error}"))?;
    Command::new(&executable)
        .arg("--uia-helper")
        .arg("--hwnd")
        .arg(format!("0x{:x}", driver.hwnd as usize))
        .arg("--pid")
        .arg(driver.pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| format!("启动 UIA helper 失败 {}: {error}", executable.display()))
}

fn spawn_uia_stdout_reader(
    stdout: impl Read + Send + 'static,
) -> (Receiver<String>, thread::JoinHandle<()>) {
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut bytes = Vec::new();
            match reader.read_until(b'\n', &mut bytes) {
                Ok(0) => break,
                Ok(_) if bytes.len() > UIA_HELPER_MAX_LINE_BYTES => {
                    let _ = sender.send(
                        json!({
                            "kind": "reader-error",
                            "stage": "helper-output-too-large",
                        })
                        .to_string(),
                    );
                }
                Ok(_) => {
                    let line = String::from_utf8_lossy(&bytes).trim().to_owned();
                    if !line.is_empty() && sender.send(line).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(
                        json!({
                            "kind": "reader-error",
                            "stage": format!("helper-output-read-error:{error}"),
                        })
                        .to_string(),
                    );
                    break;
                }
            }
        }
    });
    (receiver, reader)
}

fn apply_uia_helper_message(
    line: &str,
    last_stage: &mut String,
    partial_tree: &mut Option<Value>,
    completed_tree: &mut Option<Value>,
    helper_error: &mut Option<String>,
) -> bool {
    let value = match serde_json::from_str::<Value>(line) {
        Ok(value) => value,
        Err(error) => {
            *helper_error = Some(format!("UIA helper 输出不是合法 JSON：{error}"));
            return false;
        }
    };
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if let Some(stage) = value.get("stage").and_then(Value::as_str) {
        *last_stage = stage.to_owned();
    }
    match kind {
        "stage" => {
            if let Some(tree) = value.get("tree") {
                *partial_tree = Some(tree.clone());
            }
        }
        "error" | "reader-error" => {
            *helper_error = value
                .get("stage")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| Some("UIA helper 未完成 UIA 读取".to_owned()));
        }
        _ => {}
    }
    if kind == "stage"
        && value.get("tree").is_some()
        && value
            .get("stage")
            .and_then(Value::as_str)
            .is_some_and(|stage| stage == "complete")
    {
        *completed_tree = value.get("tree").cloned();
        return true;
    }
    false
}

fn annotate_uia_helper_result(
    mut tree: Value,
    status: &str,
    last_stage: &str,
    error: Option<String>,
) -> Value {
    if let Value::Object(object) = &mut tree {
        object.insert(
            "helper".to_owned(),
            json!({
                "status": status,
                "timeoutMs": UIA_HELPER_TIMEOUT.as_millis(),
                "lastStage": last_stage,
                "error": error,
            }),
        );
    }
    tree
}

fn accessibility_tree_fallback(
    driver: &NativeWindowDriver,
    partial_tree: Option<Value>,
    status: &str,
    last_stage: &str,
    error: Option<String>,
) -> Value {
    let mut tree = partial_tree.unwrap_or_else(|| base_accessibility_tree(driver));
    let reason = if status == "timeout" {
        "UIA helper 在有限超时内未完成；主 runner 不再同步等待，provider/subtree 保持 pending 或 unavailable"
            .to_owned()
    } else {
        error
            .clone()
            .unwrap_or_else(|| "UIA helper 未返回完整 UIA 证据".to_owned())
    };
    if let Value::Object(object) = &mut tree {
        object
            .entry("uiaHasServerSideProvider")
            .or_insert(Value::Null);
        object.entry("provider").or_insert(json!({
            "status": "pending",
            "source": "UIA helper did not return provider state",
        }));
        object.insert(
            "subtree".to_owned(),
            json!({
                "status": "unavailable",
                "reason": reason,
                "maxDepth": UIA_SUBTREE_MAX_DEPTH,
                "nodeLimit": UIA_SUBTREE_MAX_NODES,
                "textLimitChars": UIA_SUBTREE_MAX_TEXT_CHARS,
            }),
        );
    }
    annotate_uia_helper_result(tree, status, last_stage, error)
}

fn base_accessibility_tree(driver: &NativeWindowDriver) -> Value {
    json!({
        "schema": "keencode/accesskit-native-tree",
        "transport": "Windows UI Automation (AccessKit provider surface)",
        "rootHwnd": format!("0x{:x}", driver.hwnd as usize),
        "processId": driver.pid,
        "uiaHasServerSideProvider": Value::Null,
        "provider": {
            "status": "pending",
            "source": "UIA helper did not return provider state",
        },
        "uiaNodeFromHandle": "pending",
        "rootProperties": {},
        "subtree": {
            "status": "unavailable",
            "reason": "UIA helper did not return a root snapshot",
        },
        "childWindows": collect_child_windows(driver.hwnd),
        "boundary": "subtree.nodes 是通过标准 IUIAutomation TreeWalker 读取的真实控件子树；childWindows 仅是 HWND 诊断，不替代 UIA 控件树",
    })
}

fn collect_child_windows(hwnd: HWND) -> Vec<Value> {
    let mut child_state = ChildWindowState { nodes: Vec::new() };
    unsafe {
        EnumChildWindows(
            hwnd,
            Some(child_window_callback),
            &mut child_state as *mut _ as LPARAM,
        );
    }
    child_state.nodes
}

fn read_uia_subtree(root: IUIAutomationElement, walker: IUIAutomationTreeWalker) -> Value {
    let mut pending = VecDeque::new();
    pending.push_back((root, 0usize));
    let mut nodes = Vec::new();
    let mut nodes_truncated = false;

    // 逐层登记节点，并在登记前检查剩余额度，避免先整树遍历再裁剪。
    while let Some((element, depth)) = pending.pop_front() {
        if nodes.len() >= UIA_SUBTREE_MAX_NODES {
            nodes_truncated = true;
            break;
        }
        let index = nodes.len();
        nodes.push(uia_element_node(&element, index, depth));
        if depth >= UIA_SUBTREE_MAX_DEPTH {
            continue;
        }

        let Ok(mut child) = (unsafe { walker.GetFirstChildElement(&element) }) else {
            continue;
        };
        loop {
            if nodes.len().saturating_add(pending.len()) >= UIA_SUBTREE_MAX_NODES {
                nodes_truncated = true;
                break;
            }
            pending.push_back((child.clone(), depth + 1));
            if nodes.len().saturating_add(pending.len()) >= UIA_SUBTREE_MAX_NODES {
                nodes_truncated = true;
                break;
            }
            let Ok(next) = (unsafe { walker.GetNextSiblingElement(&child) }) else {
                break;
            };
            child = next;
        }
    }

    let node_count = nodes.len();
    json!({
        "status": if node_count == 0 { "empty" } else { "available" },
        "hresult": format_hresult(0),
        "maxDepth": UIA_SUBTREE_MAX_DEPTH,
        "nodeLimit": UIA_SUBTREE_MAX_NODES,
        "textLimitChars": UIA_SUBTREE_MAX_TEXT_CHARS,
        "nodes": nodes,
        "nodeCount": node_count,
        "nodesTruncated": nodes_truncated,
        "source": "Windows UI Automation IUIAutomation::ControlViewWalker with bounded child traversal",
    })
}

fn uia_element_node(element: &IUIAutomationElement, index: usize, depth: usize) -> Value {
    let mut properties = std::collections::BTreeMap::new();
    properties.insert(
        "name".to_owned(),
        uia_bstr_property(unsafe { element.CurrentName() }),
    );
    properties.insert(
        "automationId".to_owned(),
        uia_bstr_property(unsafe { element.CurrentAutomationId() }),
    );
    properties.insert(
        "className".to_owned(),
        uia_bstr_property(unsafe { element.CurrentClassName() }),
    );
    properties.insert(
        "controlType".to_owned(),
        uia_property_value(unsafe { element.CurrentControlType() }, |value| {
            json!(value.0)
        }),
    );
    properties.insert(
        "localizedControlType".to_owned(),
        uia_bstr_property(unsafe { element.CurrentLocalizedControlType() }),
    );
    properties.insert(
        "frameworkId".to_owned(),
        uia_bstr_property(unsafe { element.CurrentFrameworkId() }),
    );
    properties.insert(
        "providerDescription".to_owned(),
        uia_bstr_property(unsafe { element.CurrentProviderDescription() }),
    );
    properties.insert(
        "isControlElement".to_owned(),
        uia_bool_property(unsafe { element.CurrentIsControlElement() }),
    );
    properties.insert(
        "isContentElement".to_owned(),
        uia_bool_property(unsafe { element.CurrentIsContentElement() }),
    );
    properties.insert(
        "isEnabled".to_owned(),
        uia_bool_property(unsafe { element.CurrentIsEnabled() }),
    );
    // 读取标准 UIA 焦点属性，区分点击命中、焦点丢失和键盘事件未处理。
    properties.insert(
        "hasKeyboardFocus".to_owned(),
        uia_bool_property(unsafe { element.CurrentHasKeyboardFocus() }),
    );
    properties.insert(
        "isKeyboardFocusable".to_owned(),
        uia_bool_property(unsafe { element.CurrentIsKeyboardFocusable() }),
    );
    properties.insert(
        "isOffscreen".to_owned(),
        uia_bool_property(unsafe { element.CurrentIsOffscreen() }),
    );
    properties.insert(
        "boundingRectangle".to_owned(),
        uia_property_value(unsafe { element.CurrentBoundingRectangle() }, |value| {
            json!({
                "left": value.left,
                "top": value.top,
                "right": value.right,
                "bottom": value.bottom,
            })
        }),
    );
    properties.insert(
        "nativeWindowHandle".to_owned(),
        uia_property_value(unsafe { element.CurrentNativeWindowHandle() }, |value| {
            json!(value.0 as usize)
        }),
    );
    json!({
        "treeIndex": 0,
        "index": index,
        "depth": depth,
        "properties": properties,
    })
}

fn uia_bstr_property(result: windows::core::Result<windows::core::BSTR>) -> Value {
    uia_property_value(result, |value| {
        Value::String(bounded_utf16_text(&value, UIA_SUBTREE_MAX_TEXT_CHARS))
    })
}

fn uia_bool_property(result: windows::core::Result<windows::core::BOOL>) -> Value {
    uia_property_value(result, |value| json!(value.as_bool()))
}

fn uia_property_value<T>(
    result: windows::core::Result<T>,
    to_value: impl FnOnce(T) -> Value,
) -> Value {
    match result {
        Ok(property) => json!({
            "hresult": format_hresult(0),
            "value": to_value(property),
        }),
        Err(error) => json!({
            "hresult": format_hresult(error.code().0),
            "value": Value::Null,
        }),
    }
}

fn bounded_utf16_text(value: &[u16], max_chars: usize) -> String {
    let truncated = value.len() > max_chars;
    let mut text = String::from_utf16_lossy(&value[..value.len().min(max_chars)]);
    if truncated {
        text.push_str("...");
    }
    text
}

fn window_geometry(hwnd: HWND) -> Value {
    let mut window_rect = RECT::default();
    let mut client_rect = RECT::default();
    let mut client_origin = POINT { x: 0, y: 0 };
    let window_ok = unsafe { GetWindowRect(hwnd, &mut window_rect) != 0 };
    let client_ok = unsafe { GetClientRect(hwnd, &mut client_rect) != 0 };
    let origin_ok = unsafe { ClientToScreen(hwnd, &mut client_origin) != 0 };
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let awareness =
        unsafe { GetAwarenessFromDpiAwarenessContext(GetWindowDpiAwarenessContext(hwnd)) };
    json!({
        "windowRect": window_ok.then(|| rect_value(window_rect)),
        "clientRect": client_ok.then(|| rect_value(client_rect)),
        "clientOriginScreen": origin_ok.then(|| json!({
            "x": client_origin.x,
            "y": client_origin.y,
        })),
        "dpi": (dpi != 0).then_some(dpi),
        "dpiAwareness": dpi_awareness_name(awareness),
    })
}

fn rect_value(rect: RECT) -> Value {
    json!({
        "left": rect.left,
        "top": rect.top,
        "right": rect.right,
        "bottom": rect.bottom,
        "width": rect.right.saturating_sub(rect.left),
        "height": rect.bottom.saturating_sub(rect.top),
    })
}

fn dpi_awareness_name(value: i32) -> &'static str {
    match value {
        0 => "unaware",
        1 => "system-aware",
        2 => "per-monitor-aware",
        _ => "unknown",
    }
}

struct ComGuard;

impl ComGuard {
    fn new() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(null(), COINIT_MULTITHREADED as u32) };
        if hr < 0 {
            return Err(format!("CoInitializeEx 失败: {}", format_hresult(hr)));
        }
        Ok(Self)
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

unsafe extern "system" fn child_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = unsafe { &mut *(lparam as *mut ChildWindowState) };
    if state.nodes.len() >= 128 {
        return FALSE;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    state.nodes.push(json!({
        "hwnd": format!("0x{:x}", hwnd as usize),
        "processId": pid,
        "title": window_text(hwnd),
        "className": class_name(hwnd),
        "visible": unsafe { IsWindowVisible(hwnd) != 0 },
    }));
    TRUE
}

fn send_inputs(inputs: &[INPUT]) -> Result<(), String> {
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };
    if sent != inputs.len() as u32 {
        return Err(format!("SendInput 只注入 {sent}/{} 个输入", inputs.len()));
    }
    Ok(())
}

fn left_click_inputs() -> [INPUT; 2] {
    [
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_LEFTDOWN,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_LEFTUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    ]
}

fn validate_scroll_point(x: i32, y: i32, width: u32, height: u32) -> Result<(), String> {
    if x < 0 || u32::try_from(x).unwrap_or(u32::MAX) >= width {
        return Err(format!(
            "scroll x 坐标越出 client 边界：point=({x},{y}) client={width}x{height}"
        ));
    }
    if y < 0 || u32::try_from(y).unwrap_or(u32::MAX) >= height {
        return Err(format!(
            "scroll y 坐标越出 client 边界：point=({x},{y}) client={width}x{height}"
        ));
    }
    Ok(())
}

fn mouse_wheel_input(delta_y: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: delta_y as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn keyboard_input(value: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: if flags & KEYEVENTF_UNICODE != 0 {
                    0
                } else {
                    value as VIRTUAL_KEY
                },
                wScan: if flags & KEYEVENTF_UNICODE != 0 {
                    value
                } else {
                    0
                },
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn modifier_to_vk(value: &str) -> Result<VIRTUAL_KEY, String> {
    match value.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Ok(0xA2),
        "shift" => Ok(0xA0),
        "alt" => Ok(0xA4),
        "win" | "windows" => Ok(0x5B),
        _ => Err(format!("未知键盘修饰键 {value}")),
    }
}

fn key_to_vk(value: &str) -> Result<VIRTUAL_KEY, String> {
    let lower = value.to_ascii_lowercase();
    let key = match lower.as_str() {
        "enter" | "return" => 0x0D,
        "esc" | "escape" => 0x1B,
        "tab" => 0x09,
        "backspace" | "back" => 0x08,
        "space" => 0x20,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "home" => 0x24,
        "end" => 0x23,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "f1" => 0x70,
        "f2" => 0x71,
        "f3" => 0x72,
        "f4" => 0x73,
        "f5" => 0x74,
        "f6" => 0x75,
        "f7" => 0x76,
        "f8" => 0x77,
        "f9" => 0x78,
        "f10" => 0x79,
        "f11" => 0x7A,
        "f12" => 0x7B,
        _ if lower.len() == 1 && lower.as_bytes()[0].is_ascii_alphanumeric() => {
            // 虚拟键码中的字母使用大写 ASCII；小写 ASCII 值会被解释为功能键范围。
            lower.as_bytes()[0].to_ascii_uppercase() as u16
        }
        _ if lower.starts_with("0x") => {
            u16::from_str_radix(&lower[2..], 16).map_err(|_| format!("无效虚拟键 {value}"))?
        }
        _ => return Err(format!("未知键盘键 {value}")),
    };
    Ok(key)
}

fn window_text(hwnd: HWND) -> Option<String> {
    let mut buffer = [0u16; 512];
    let length = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    (length > 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
}

fn class_name(hwnd: HWND) -> Option<String> {
    let mut buffer = [0u16; 256];
    let length = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    (length > 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
}

fn variant_value(variant: &VARIANT) -> Value {
    variant_value_limited(variant, usize::MAX)
}

fn variant_value_limited(variant: &VARIANT, max_text_chars: usize) -> Value {
    let inner = unsafe { variant.Anonymous.Anonymous };
    match inner.vt & VT_TYPEMASK {
        VT_BSTR => {
            let bstr = unsafe { inner.Anonymous.bstrVal };
            if bstr.is_null() {
                Value::Null
            } else {
                let length = unsafe { windows_sys::Win32::Foundation::SysStringLen(bstr) } as usize;
                let text = unsafe { std::slice::from_raw_parts(bstr, length) };
                Value::String(bounded_utf16_text(text, max_text_chars))
            }
        }
        VT_I4 => json!(unsafe { inner.Anonymous.lVal }),
        VT_I8 => json!(unsafe { inner.Anonymous.llVal }),
        VT_UI4 => json!(unsafe { inner.Anonymous.ulVal }),
        VT_UI8 => json!(unsafe { inner.Anonymous.ullVal }),
        VT_R8 => json!(unsafe { inner.Anonymous.dblVal }),
        VT_BOOL => json!(unsafe { inner.Anonymous.boolVal != 0 }),
        VT_LPWSTR => Value::String("<LPWSTR>".to_owned()),
        _ => json!({"variantType": inner.vt}),
    }
}

fn format_hresult(value: i32) -> String {
    format!("0x{:08x}", value as u32)
}

fn filetime_ms(value: FILETIME) -> u64 {
    let ticks = ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64;
    ticks / 10_000
}

fn write_bmp(path: &Path, width: u32, height: u32, pixels: &[u8]) -> Result<(), String> {
    let image_bytes = pixels.len() as u32;
    let file_size = 54u32.saturating_add(image_bytes);
    let mut bytes = Vec::with_capacity(file_size as usize);
    bytes.extend_from_slice(b"BM");
    bytes.extend_from_slice(&file_size.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&54u32.to_le_bytes());
    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&(width as i32).to_le_bytes());
    bytes.extend_from_slice(&(-(height as i32)).to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&image_bytes.to_le_bytes());
    bytes.extend_from_slice(&[0; 16]);
    bytes.extend_from_slice(pixels);
    fs::write(path, bytes).map_err(|error| format!("写入截图失败 {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{key_to_vk, validate_scroll_point};

    #[test]
    fn single_letter_keys_use_uppercase_virtual_key_codes() {
        assert_eq!(key_to_vk("q"), Ok(0x51));
        assert_eq!(key_to_vk("Q"), Ok(0x51));
        assert_eq!(key_to_vk("a"), Ok(0x41));
        assert_eq!(key_to_vk("7"), Ok(0x37));
        assert_eq!(key_to_vk("f2"), Ok(0x71));
    }

    #[test]
    fn scroll_points_must_stay_inside_the_client() {
        assert!(validate_scroll_point(0, 0, 100, 100).is_ok());
        assert!(validate_scroll_point(99, 99, 100, 100).is_ok());
        assert!(validate_scroll_point(-1, 0, 100, 100).is_err());
        assert!(validate_scroll_point(0, 100, 100, 100).is_err());
    }
}
