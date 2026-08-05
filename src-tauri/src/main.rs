fn main() {
    // 在 WebView 创建前应用需要重启生效的浏览器参数。
    keencode_desktop::configure_before_start();
    // 启动 KeenCode 的 Tauri 本地后端。
    keencode_desktop::run();
}
