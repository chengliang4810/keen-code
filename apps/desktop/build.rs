fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // GPUI 的 Windows 平台资源提供唯一 manifest（PerMonitorV2、SegmentHeap、
        // CommonControls）；这里仅补充应用图标和版本资源，避免重复嵌入 manifest。
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("icons/icon.ico")
            .set("ProductName", "KeenCode")
            .set("FileDescription", "KeenCode 原生 AI 编程工作台");
        resource.compile().expect("Windows 原生资源编译失败");
    }
}
