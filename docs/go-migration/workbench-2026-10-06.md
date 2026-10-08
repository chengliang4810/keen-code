# 基础三栏工作台验收

日期：2026-10-06。结论：指定三栏结构及右侧真实功能已实现，最终 Go 全量测试、vet、相关 race 与 macOS 原生验收通过。搜索、自动化、自定义分组未加入。未提交、未推送。

## 基线与范围

- KeenCode分支 `go-gpui`，HEAD `d51a7116563ae2f114da93f43900d9b5a2ee8cb8`；Go代码为未跟踪文件。旧React/Tauri/Rust栈未修改。
- 修改前快照：[source-before](../../output/acceptance/2026-10-06/three-column/source-before/)，已有文件变更：[source-changes.diff.txt](../../output/acceptance/2026-10-06/three-column/source-changes.diff.txt)。新增文件直接审查源码。
- ZCode：`/Users/chengliang/code-repositories/ZCode`，固定提交 `872ad960de7ec172591f7e1952f7849229f94521`，Apache-2.0；研究Web源码结构并移植为mygo原生控件，未复制React实现。
- 参考 `packages/ui/src/` 下的 `app-shell/WorkspaceShellLayout.tsx`、`WorkspaceSidebar.tsx`、`WorkspacePinnedTasksSection.tsx`、`WorkspaceSidebar/WorkspacePurposeSection.tsx`、`app-shell/AnimatedSidePanePanel.tsx`、`app-shell/SidePaneTabOverview.tsx`、`workspace-file-tree/`、`SidePaneTerminalPane.tsx`。
- 延续既有Go主题和UI kit；新增Lucide图标使用 `lucide-static@1.52.0`，许可见 `internal/ui/kit/svg/LICENSE.lucide`。

## 产品规则与实现

| 区域 | 行为 |
| --- | --- |
| 左栏 | 264 DIP；顶部新建对话，依次置顶、项目、对话，底部设置；可收起 |
| 置顶 | 顶栏或对话行右键切换；runtime session meta持有权威值，重启恢复；不改变会话更新时间 |
| 项目 | 添加目录、展开项目内对话、在项目中新建；canonical目录持久化到workspaces.json；保存失败不替换内存 |
| 对话 | 未置顶且不属于已登记项目的对话；置顶行不重复出现；移除项目不删除对话 |
| 中栏 | 现有会话、消息、composer与模型/工具能力；会话列最小360 DIP |
| 右栏 | 默认360 DIP、最小280 DIP；复用ui.Split，可拖动与键盘调整；终端、差异、文件标签与打开入口 |
| 窄窗 | 1200 DIP以下默认收右栏；显式打开且窗口小于1080 DIP时暂时隐藏左栏；可返回左栏；标题栏避让原生窗控 |
| 生命周期 | 按需读取/启动，无空闲轮询；切对话（含同目录不同对话）取消旧任务、丢弃旧结果、释放旧PTY；关闭右栏/窗口释放PTY |

右栏使用当前对话的项目目录，草稿使用所选目录。没有目录时提示选择项目，不启动终端或读取其他目录。

- 终端：复用mygo terminal plugin的Ghostty/PTY，用户shell在当前目录启动，支持退出后重启。
- 差异：真实Git未暂存、已暂存、未跟踪文件；未跟踪文件可转预览；提供刷新。禁用external diff/textconv，10秒超时，每次输出最多2 MiB，每节最多显示2000行。
- 文件：原生树控件逐层展开、UTF-8文本预览和选中文字；os.Root限制项目外路径与symlink越界。单目录最多2000项、最多32层、预览最多1 MiB，拒绝二进制和非普通文件。

布局、导航投影、异步读取分别在 `internal/app/{workbench,navigation,inspector}.go`；系统读取位于 `internal/workspace/`；项目配置与pin分别留在config/runtime。

## 最终验证

环境：Go1.27.1，macOS14.8.7（23J520）、Intel x86_64；原生截图2×。mygo基线 `1ff0c41aeea303df5f948dfdfc4e6bbeb6a46df0` 加两个本地Darwin补丁，见 [补丁说明](patches/README.md)。

| 检查 | 结果与证据 |
| --- | --- |
| `go test ./... -count=1` | 通过，[go-test.log](../../output/acceptance/2026-10-06/three-column/go-test.log) |
| `go vet ./...` | 通过，无诊断，[go-vet.log](../../output/acceptance/2026-10-06/three-column/go-vet.log) |
| `go test -race ./internal/app ./internal/runtime ./internal/workspace ./internal/config -count=1` | 通过，[go-race.log](../../output/acceptance/2026-10-06/three-column/go-race.log) |
| mygo目录 `go test ./plugins/terminal ./plugins/terminal/internal/pty -count=1` | 通过，[mygo-terminal-test.log](../../output/acceptance/2026-10-06/three-column/mygo-terminal-test.log) |
| macOS真实原生窗口 | PASS，[result.json](../../output/acceptance/2026-10-06/three-column/native/result.json)、[native-run.log](../../output/acceptance/2026-10-06/three-column/native-run.log) |
| 格式与源码审查 | gofmt无输出；两个仓库git diff --check、两个依赖补丁反向apply --check通过；未跟踪文本另行检查空白 |

回归覆盖pin重启、导航排重、移除项目保留会话、保存失败、真实Git/文件读取、旧工作区及同目录旧对话结果丢弃、亮暗720/1280/1920几何、窄窗避让窗控、关闭右栏不启动隐藏终端。

原生脚本使用完全隔离的临时数据、Git项目与显式聊天夹具，不需要模型凭据。聊天截图不能作为本轮真实模型回复证据；模型/六工具闭环属于 [上一轮验收](acceptance-2026-10-06.md)。

原生实际场景：导航和置顶落盘；真实Git差异；展开嵌套目录并读取文件；向PTY粘贴命令，在当前目录生成内容精确为 `WORKBENCH_PTY_OK` 的文件；取得shell PID并确认关闭面板后进程消失；720×490切换右栏/左栏；底部打开设置；重启恢复pin与项目，切亮色主题。退出清理自身进程、临时目录并恢复剪贴板。截图按PID筛选原生窗口。

```sh
python3 docs/go-migration/scripts/workbench-native.py \
  --output output/acceptance/2026-10-06/three-column/native
```

## 原生截图

1280×800 DIP（2560×1600像素），相同原生环境：

- [暗色三栏](../../output/acceptance/2026-10-06/three-column/native/01-three-columns-dark.png)
- [暗色Git差异](../../output/acceptance/2026-10-06/three-column/native/02-git-diff-dark.png)
- [暗色文件预览](../../output/acceptance/2026-10-06/three-column/native/03-file-preview-dark.png)
- [真实终端](../../output/acceptance/2026-10-06/three-column/native/04-terminal-dark.png)
- [亮色三栏](../../output/acceptance/2026-10-06/three-column/native/06-three-columns-light.png)
- [亮色Git差异](../../output/acceptance/2026-10-06/three-column/native/07-git-diff-light.png)

720×490 DIP（1440×980像素）：[窄窗右栏与窗控避让](../../output/acceptance/2026-10-06/three-column/native/05-narrow-side-pane.png)。这是关闭终端后再次展开面板的截图，终端正在重新启动；实际命令证据见04。

## 已知边界与下一步

1. 新增Darwin PTY修复已应用本机并保存补丁，尚未发布依赖版本；go.mod仍使用绝对本地replace。干净机器/CI和发布依赖闭环未完成。mygo targeted测试通过不等于其全仓全绿。
2. 本轮完成指定基础布局及真实右栏；未重建同提交、同状态的可运行ZCode基线，也未计算对应组件像素误差。vibrancy、Tahoe圆角、动画、composer完整细节仍有差异，不能称完整像素复刻。
3. diff为只读统一差异，文件为只读预览；未实现双栏diff、编辑、语法高亮、多终端。右栏宽度仅内存保存，左栏固定宽度。
4. Windows原生、其他IME、长会话/完整设置矩阵与性能预算未验收。继续优先处理可发布依赖和交接文档中的视觉差异，避免重写已通过的三栏与核心会话能力。
