#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 产品入口仅创建 Rust Runtime 与 GPUI 窗口。
    keencode_desktop::run();
}
