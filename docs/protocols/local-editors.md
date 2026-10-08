# Native GPUI 本地编辑器契约

本文描述当前原生工作台的系统编辑器发现与打开边界。生产入口是
`NativeWorkbench.editors: EditorService`，由 `NativeWorkbenchPanel` 直接调用；没有
Tauri command、`IPlatformService`、`getApplicationIcon` 或前端启动器。

实现锚点：`apps/desktop/src/native_ui/workbench/editors.rs`、
`apps/desktop/src/native_ui/workbench/panel.rs`、
`apps/desktop/src/native_ui/workbench.rs` 和
`apps/desktop/src/native_host/launch.rs`。

## Typed service API

`EditorService::discover()` 返回 `Vec<EditorInfo>`，每项字段为：

```text
EditorInfo {
  id,
  name,
  executable,
  is_file_manager,
}
```

`EditorService::open(editor_id, path)` 先 canonicalize 并确认目标存在，再从本次
`discover()` 结果中按稳定 `editor_id` 选择可执行文件，最后通过参数数组启动进程，
返回：

```text
OpenEditorResult { editor_id, path }
```

未知或未安装的 editor ID、不可访问目标和启动失败都返回 `Err(String)`。Shell 不参与
参数解析；Windows 使用隐藏 console flag，文件管理器选中文件时传单个
`/select,<path>` 参数，目录直接传路径。macOS Finder 使用 `open -R` 选择文件或
`open` 打开目录。

## 发现规则

- Windows 始终尝试系统 `explorer.exe`；VS Code、Cursor 和 Zed 从已知安装路径或
  `PATH` 发现。
- macOS 始终提供 `/usr/bin/open` 作为 Finder；VS Code、Cursor 和 Zed 从
  `/Applications` 或用户 `Applications` 目录发现。
- 其他 Unix 只从 `PATH` 发现 VS Code、Cursor 和 Zed，不伪造 Finder/Explorer。
- 当前 `EditorInfo` 暴露的是可执行路径，不生成 icon data URL。图标或品牌展示若要
  增加，必须先扩展真实 Rust 类型和 GPUI 投影，不能恢复旧前端 icon API。

工作台的 `open_external_editor` 只从当前项目快照选择已发现的 editor，并把路径交给
上述 service；service 自身负责 canonicalize、存在性和 editor identity 校验，面板负责
把操作放入 blocking 任务、检查项目 generation 并显示成功/失败状态。它不会把任意
用户输入的 executable 当作身份，也不接受 remote target。

## 历史：Tauri 编辑器 API（已退役）

旧材料中的 `IPlatformService.getInstalledEditors()`、`getApplicationIcon()`、
`openInEditor()`、`EditorInfo { id, name, iconDataUrl }` 以及 remote target 拒绝规则
属于 Tauri/Source 适配层的历史接口。它们不描述当前 `EditorService` 的 Rust 结构，
不能作为新增 GPUI API 或验收入口。旧的本机发现证据仍可用于解释历史验收，但新证据
应记录 `EditorService::discover/open` 和 `NativeWorkbenchPanel` 的真实调用链。

### 历史验收证据（状态保留）

旧记录中的依赖和本机候选结论仍保留为历史证据：Windows HICON 不是 PNG 字节；当时
保留 `png = 0.18.1`，并以 `cargo tree -p keencode-desktop --edges normal -i
png@0.18.1` 核对过直接依赖。2026-10-03 的 Windows 候选扫描记录在
`tooling/native-live/native-editor-discovery-evidence.json`，当时仅发现 `explorer`，
VS Code、VS Code Insiders、Cursor、Zed 的已知安装路径和 PATH 均为空；旧 native-live
计划因此只启动真实 Explorer，没有冒充不存在的 IDE。上述结论不改变当前
`EditorService` 的 typed 字段，也不代表本轮重新执行了本机验收。
