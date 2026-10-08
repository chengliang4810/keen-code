# KeenCode → Go（mygo 原生 UI）迁移交接

日期：2026-10-06。仓库 `/Users/chengliang/Documents/jian-desktop`，分支 `go-gpui`，基线 HEAD `d51a7116`。

**当前结论：基础三栏已实现，左侧新建对话、置顶、项目、对话、底部设置；中间沿用具体对话；右侧接入真实终端、Git 差异和文件浏览。最终 Go 检查和 macOS 原生三栏验收通过。** 不含搜索、自动化和自定义分组。首版模型与工具闭环的上一轮验收仍见原报告；本轮聊天使用显式夹具，不重复宣称真实模型验收。UI 按 ZCode 源码继续对齐，尚未完成全量像素复刻或 Windows 验收。本轮没有提交或推送。Go 迁移文件仍为未跟踪文件，原有 `apps/`、`core/`、`tooling/` 保持只读。

## 1. 继续工作的目标

优先保证 Go + mygo 原生应用的真实功能闭环，再按 ZCode 组件、令牌、布局逐块复刻。旧 React/Tauri/Rust 栈仅作行为参考；浏览器/headless 截图不能替代原生窗口验收。后续能力不与本轮视觉工作混做。

## 2. 本轮已经完成

| 范围 | 当前结果 |
| --- | --- |
| 生产装配的集成 E2E | Messages、Chat Completions 均经过真实 HTTP/SSE adapter→Agent Loop→Write/Read/Edit/Glob/Grep/Bash→结果回填→journal→UI；验证实际文件内容与重启恢复 |
| 失败路径 | 权限拒绝不落文件、HTTP400错误详情、Esc取消流式回合；有回归测试 |
| 工作区路径 | `BindInvocation` 比较 canonical 路径；同工作区 symlink 和 macOS `/tmp` 别名可用，其他工作区仍拒绝 |
| 模型选择 | 首次配置可选默认；替换/删除模型后清理失效选择；删除最后供应商后禁用发送并保留管理入口；Upsert保存成功才替换内存状态 |
| 编辑器 | 中部 Enter发送完整草稿、Shift+Enter换行；输入壳包含工具条和发送钮；20 DIP正文行高、160 DIP上限；占位符恢复绘制 |
| 聊天与外壳 | 正文进入用户气泡；空态按29dvh留白、问候上限30 DIP；主面板4 DIP inset、边框与平台圆角；新建入口左对齐；设置返回区域避让窗控 |
| 模型菜单 | 高度受窗口和320 DIP上限约束，使用已有Scroll；键盘高亮只在变化时滚入可见区；60项键盘到末项的回归通过 |
| 回合终态 | 修复 Running提前变false的竞态：终态先落盘，再允许下一轮；新增连续失败回合回归与race检查 |
| 原生依赖 | mygo resize父类递归与IME组合态按键抢占修复已应用本地，补丁和许可保存在 `patches/` |
| 三栏工作台 | 默认264 DIP左栏、可调整右栏默认360 DIP；窄窗口可展开右栏并暂时隐藏左栏，标题栏避让原生窗控 |
| 导航与持久化 | pin写入权威session meta；项目列表写入workspaces.json；置顶、项目内与其他对话不重复显示；移除项目保留对话 |
| 右侧能力 | 按需启动原生PTY；Git未暂存/已暂存/未跟踪；真实文件树与UTF-8预览；切对话丢弃旧读取，关面板/退出释放PTY |
| Darwin PTY | 修复master读取EAGAIN导致shell退出的问题；terminal/PTY targeted测试和真实命令执行、进程释放通过；增加第二个补丁 |

原生验收脚本、截图与具体检查结果见 [本轮验收报告](acceptance-2026-10-06.md)。脚本使用临时数据目录与工作区，复制供应商配置为0600；不改用户原配置，并在退出时恢复剪贴板与输入源、结束自己启动的进程。

最新三栏详见 [工作台验收报告](workbench-2026-10-06.md)，脚本 `scripts/workbench-native.py` 无需模型凭据，使用临时项目、Git仓库和显式聊天夹具。亮暗1280×800、窄720×490原生截图均保存到 `output/acceptance/2026-10-06/three-column/native/`。修复后最终全量Go测试、vet、相关race通过。

## 3. 修正上一版交接的判断

上一版§4第5项要求主区 `#202020`，这个结论错误。ZCode固定源码的 `WorkspaceShellLayout.tsx:1672` 使用 `bg-background`：zai-dark=`#161616`、zai-light=`#f8f8f8`。`--color-panel/header` 不能替代会话主面板的真实调用点。

侧栏 `<aside>` 未设背景，macOS 透出 `DesktopWindowFrame` 的半透明 background-alt与vibrancy；Go目前仍画不透明Sidebar，暗色侧栏与ZCode明显不同。底色规格已经同步纠正，不能把它描述为原5条规格全部原样通过。

## 4. 下一轮必须先处理的视觉差异

1. 使用固定源码重建可运行ZCode基线；本轮安装版3.14.4截图可作外壳参考，但不等于固定提交的产物。准备相同主题、项目、模型和空态，保存相同窗口与2×截图，再逐组件比对。
2. 接入macOS原生vibrancy与正确窗口表面层。mygo `WindowOptions.Vibrancy` 支持原生UI，不应预设它没有能力；先验证根画布的透明背景与侧栏叠色。补Tahoe12 DIP圆角；Windows5 DIP已有代码但待真机。
3. 补composer项目/权限行、模型分组与图标，按原组件结构定位。当前仅模型工具条可用，不能靠增加无功能按钮假装完整复刻。
4. 处理编辑器至少三行造成的60 DIP最小高度；ZCode最小40。先评估mygo现有排版/高度能力，不用字符数估算换行。
5. 补时间分段问候、20–30自适应字号、品牌资源和建议入口；细化工具/推理/Markdown层级与动作区。继续记录尚未对齐的动效、mask和语法高亮差异。
6. 基础三栏、窄窗口切换、底部设置、亮暗导航恢复本轮已原生验证；继续长会话滚动、长模型名与全设置页矩阵。右栏宽度目前仅本次运行内保存，左栏固定264 DIP；ZCode的动效、双栏diff与语法高亮尚未移植。

## 5. 功能验收的剩余范围

- 原生文件/目录选择、会话切换与草稿、重命名/删除、工具拒绝、错误详情与取消等全产品矩阵仍应继续逐项走；本轮生产装配E2E和原生冒烟各自覆盖范围见报告，不能混称全部手工通过。
- Windows原生窗口、IME、权限与工具执行尚未验收；macOS本轮使用搜狗拼音，其他输入法仍待测。
- 性能预算（安装包50 MB、空闲CPU<1%、冷启动1.5秒）未测。不要从test/build耗时推断启动性能。
- 可发布依赖仍未闭环：`go.mod` 使用本地mygo replace。必须发布/锁定含修复版本并在干净机器与CI验证，而不是只保留本机补丁。

## 6. 代码与工件入口

| 入口 | 职责 |
| --- | --- |
| `cmd/keencode/main.go` | 配置、runtime、窗口装配 |
| `internal/app/{app,shell,chat,settings,services}.go` | 原生外壳、界面投影、服务与设置 |
| `internal/app/{workbench,navigation,inspector}.go`、`workbench_test.go` | 三栏、导航、右侧真实能力与回归 |
| `internal/workspace/`、`internal/config/workspaces.go`、`internal/runtime/pin_test.go` | 有界文件/Git读取、项目列表与权威pin |
| `internal/app/stack_e2e_test.go` | 生产装配的HTTP/SSE与六工具闭环 |
| `internal/app/migration_regression_test.go` | 模型选择、亮暗/多尺寸composer几何 |
| `internal/runtime/session.go`、`session_test.go` | 终态落盘与Running生命周期 |
| `internal/tools/environment.go`、`invocation_regression_test.go` | canonical工作区边界 |
| `internal/ui/kit/{editor,message,select,chatlist}.go` | 输入、气泡、菜单与时间线 |
| `docs/go-migration/scripts/native-acceptance.py` | 可复现macOS原生验收 |
| `docs/go-migration/scripts/workbench-native.py` | 三栏、真实PTY、Git、文件、窄窗与重启验收 |
| `docs/go-migration/patches/` | mygo原生事件/PTY补丁、应用方法、MIT许可 |
| `output/acceptance/2026-10-06/` | 本轮源码快照、原生截图、日志与采样；git忽略 |

`git diff`不会展示未跟踪Go文件。上一轮UI变更可对照 `source-before/*.go.txt`，本次三栏对照 `three-column/source-before/*.go.txt` 与 `three-column/source-changes.diff.txt`；新增文件需直接审查。不要把 `.go` 快照放入output，否则 `go test ./...`也会扫描。根目录原有 `keencode` 二进制仍保留，不代表当前源码构建结果。

## 7. 环境与复现

- Go1.27.1，macOS Sonoma/Darwin23.6.0 Intel。
- mygo：`/Users/chengliang/code-projects/mygo`，基线 `1ff0c41aeea303df5f948dfdfc4e6bbeb6a46df0`，修改 `internal/darwin/surface.go` 与 `plugins/terminal/internal/pty/pty_darwin.go`。换机器按 `patches/README.md` 应用两个补丁并调整replace。
- ZCode源码：`/Users/chengliang/code-repositories/ZCode`，固定提交 `872ad960de7ec172591f7e1952f7849229f94521`，Apache-2.0。安装版视觉参考为3.14.4。
- macOS自动化需要辅助功能和屏幕录制授权；截图按正确窗口ID，不用屏幕区域抓图。原生验收单独运行，避免其他窗口测试争抢焦点。

```sh
go build ./...
go test ./... -count=1
go test -race ./internal/app ./internal/runtime ./internal/tools ./internal/workspace ./internal/config -count=1
go vet ./...
gofmt -l cmd internal
git diff --check

python3 docs/go-migration/scripts/workbench-native.py \
  --output output/acceptance/2026-10-06/three-column/native

python3 docs/go-migration/scripts/native-acceptance.py \
  --providers /path/to/providers.json \
  --output output/acceptance/native

# 仅当机器安装并启用搜狗拼音时添加此参数：
# --ime-source com.sogou.inputmethod.sogou.pinyin

go test -tags acceptance -run TestLive -v ./internal/agent/
```

本轮禁止将安装版不同状态的整屏像素差描述成复刻误差，也禁止把截图存在、脚本运行过或HTTP回了文本当成工具成功。必须核对实际文件、journal、UI与实际窗口尺寸。
