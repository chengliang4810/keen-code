# 本地编辑器平台契约

`IPlatformService.getInstalledEditors()`、`getApplicationIcon()` 和
`openInEditor()` 在 Tauri 中由 `apps/desktop/src/ui_editors.rs` 提供。Rust 是唯一
的发现与启动边界，前端只消费 `EditorInfo { id, name, iconDataUrl }`，不执行命令，
也不把用户传入的 executable 当作编辑器身份。

## 发现

- Windows 始终按系统位置发现 `explorer`，并从已知安装目录或 `PATH` 发现
  `vscode`、`vscode-insiders`、`cursor`、`zed`。
- macOS 始终按 `/usr/bin/open` 提供 `finder`，并从 `/Applications` 或用户
  `Applications` 目录发现相同的本地编辑器。
- 其他 Unix 仅从 `PATH` 发现上述编辑器，不伪造 Finder/Explorer。
- 图标优先使用 Windows Shell 的真实 HICON；系统 API 不可用时返回稳定的
  editorId 回退 SVG data URL，不能影响发现或启动结果。
- `getApplicationIcon` 的 macOS bundle ID 只映射到已发现的本地编辑器；Windows
  executable locator 也必须 canonicalize 后命中同一发现结果。

## 启动与授权

`openInEditor` 只接受已发现的 editorId 和已登记 workspace 根内的现有绝对路径。
Rust 先 canonicalize 并检查 `pathKind`，再使用参数数组启动进程；文件名中的 `&`、
引号或空格不会被交给 shell。Windows Explorer 文件使用单个 `/select,<path>` 参数，
目录直接传路径；macOS Finder 使用 `open -R` 或 `open`。

远程 `remoteTarget`、未知 editorId、相对路径和 workspace 根外路径均明确拒绝。正常
产品启动使用编辑器的标准用户配置；native-live 计划只验证隔离项目与参数边界，
不会向真实编辑器注入任意 executable 或覆盖用户编辑器配置。

## 依赖与本机证据

Windows HICON 不是 PNG 字节，当前依赖图中没有可直接写 PNG data URL 的桌面自有
编码器，因此保留 `png = 0.18.1` 是实现真实 Windows 图标所需的最小直接依赖；
`cargo tree -p keencode-desktop --edges normal -i png@0.18.1` 实测只显示
`png v0.18.1 -> keencode-desktop`。本机 Cargo registry 中该版本源文件为 40 个、
合计 565,549 bytes；这是源码缓存大小，不等同于最终产品体积。最终 release 二进制
增量由 product-size 构建单独测量，当前没有用估算替代该结果。

2026-10-03 Windows 实际候选扫描结果保存在
`tooling/native-live/native-editor-discovery-evidence.json`：本机仅发现
`explorer`，VS Code、VS Code Insiders、Cursor、Zed 的已知安装路径和 PATH 均为空。
因此当前 native-live 计划只启动真实 Explorer，不冒充不存在的 IDE 验收。
