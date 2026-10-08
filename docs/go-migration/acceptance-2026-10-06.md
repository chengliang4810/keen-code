# Go 原生端到端与 ZCode UI 对标验收

日期：2026-10-06。**首版核心功能闭环通过本轮验收；UI 已修复明显布局和输入问题，但完整复刻、Windows 与发布验收仍未完成。** 没有提交或推送。

## 1. 基线与环境

| 项目 | 本轮记录 |
| --- | --- |
| KeenCode | 分支 `go-gpui`，HEAD `d51a7116`；Go 迁移文件为未跟踪文件 |
| 编辑前源码 | [source-before](../../output/acceptance/2026-10-06/source-before/) 下的九份 `.go.txt` 快照；[本轮 UI 差异](../../output/acceptance/2026-10-06/review-ui.diff) |
| 系统 | macOS Sonoma 14.8.7，Darwin 23.6.0，Intel；Go 1.27.1 darwin/amd64 |
| mygo | `/Users/chengliang/code-projects/mygo`，基线 `1ff0c41aeea303df5f948dfdfc4e6bbeb6a46df0`，本地原生事件补丁 |
| ZCode 源码 | `/Users/chengliang/code-repositories/ZCode`，固定提交 `872ad960de7ec172591f7e1952f7849229f94521`，Apache-2.0 |
| ZCode 运行参考 | 安装版 3.14.4；固定源码清单为 3.14.0，没有完成固定提交重建 |
| 原生窗口 | 常规 1280×800 DIP / 2560×1600 像素；小窗口实测 720×490 DIP / 1440×980 像素 |

原生截图通过 PID、layer=0、onscreen 和窗口名筛选真实窗口，再按窗口 ID 截取。小窗口请求高度为480，但原生最小尺寸限制到490；结果文件已断言实际尺寸。Headless 的720×480布局测试不能代替这个原生结果。

## 2. 生产装配集成测试

入口：[stack_e2e_test.go](../../internal/app/stack_e2e_test.go)。使用生产 `Services.RuntimeOptions`、真实 HTTP/SSE adapter、Agent Loop、工具、journal 和 UI 投影；模型端点由本地 HTTP server 控制，权限回答由测试控制，UI 使用 mygo Tester。没有以 scripted runner 替代整个执行链，也不将此项当作真实模型或原生弹窗验收。

| 场景 | 断言与结果 |
| --- | --- |
| Messages 六工具闭环 | Write→Read→Edit→Glob→Grep→Bash；实际文件为 `AFTER`，六项工具 completed，结果进入后续模型请求，UI 显示 `STACK_OK`；重建 Services/Manager 后会话恢复，通过 |
| Chat Completions 六工具闭环 | 相同链路和断言，通过 |
| 拒绝 Write | 实际文件不存在，journal 为 denied，拒绝结果回填后仍可完成回复，通过 |
| HTTP400 | 错误终态与错误详情投影，通过 |
| Esc 取消 SSE | 请求取消、回合终态 cancelled、UI 退出运行状态，通过 |

新增回归覆盖 canonical 工作区边界、模型替换/末个供应商删除、保存失败不修改内存状态、正文位于气泡内部、中部 Enter、长菜单键盘到末项，以及亮暗三种窗口几何和长草稿输入上限。

## 3. macOS 真实原生验收

入口：[native-acceptance.py](scripts/native-acceptance.py)。最新结果为 [final-native/result.json](../../output/acceptance/2026-10-06/final-native/result.json)，`status=PASS`。

| 场景 | 实际检查 |
| --- | --- |
| 模型菜单 | 原生触发、键盘选择后回显 |
| Shift+Enter | 中文草稿换行，尚未创建/发送会话 |
| 中部 Enter | 在文本中间按回车发送完整原草稿，真实模型返回 `NATIVE_OK` |
| Write 权限 | 真实模型调用 Write，原生权限弹窗选择允许一次 |
| 工具成功 | 实际文件内容精确等于 `GO_NATIVE_TOOL_OK`，journal 的 tool_end/tool_result 为 completed |
| 重启恢复 | 结束并重新启动原生进程，恢复已完成回复 |
| 主题 | 设置中切换暗/亮主题，检查原生界面和落盘值 |
| 窗口缩放 | 缩小后无 resize 崩溃，断言真实窗口720×490 DIP |
| 搜狗拼音 | 物理 `nihao` 形成 marked text；Enter提交组合态且不发送；Space提交“你好”且不发送 |

输入法检查先确认 marked text 时 AXValue 仍为空，防止直接输入拉丁字母被误当成组合输入。仅搜狗拼音已验收，其他输入法待测。

脚本将供应商配置复制到临时目录并设0600，使用临时工作区，不改用户原配置；退出时恢复剪贴板、输入源、脚本改变的搜狗中英文状态，结束自己启动的进程并清除临时目录。日志不包含供应商凭据。

复现命令（供应商文件由执行者提供）：

```sh
python3 docs/go-migration/scripts/native-acceptance.py \
  --providers /path/to/providers.json \
  --ime-source com.sogou.inputmethod.sogou.pinyin \
  --output output/acceptance/native
```

没有搜狗拼音时省略 `--ime-source`，该运行不能据此声称 IME 已通过。自动化需要系统辅助功能和屏幕录制权限；原生验收不要与其他创建窗口的测试同时运行。

截图均位于 [final-native](../../output/acceptance/2026-10-06/final-native/)：

| 截图 | 状态 |
| --- | --- |
| [01-draft-dark.png](../../output/acceptance/2026-10-06/final-native/01-draft-dark.png) | 暗色草稿、占位符、模型工具条 |
| [02-model-menu.png](../../output/acceptance/2026-10-06/final-native/02-model-menu.png) | 模型菜单 |
| [02-shift-enter.png](../../output/acceptance/2026-10-06/final-native/02-shift-enter.png) | 换行草稿 |
| [02-ime-marked.png](../../output/acceptance/2026-10-06/final-native/02-ime-marked.png) / [02-ime-committed.png](../../output/acceptance/2026-10-06/final-native/02-ime-committed.png) / [02-ime-chinese.png](../../output/acceptance/2026-10-06/final-native/02-ime-chinese.png) | 组合态、Enter提交、中文提交 |
| [03-reply-dark.png](../../output/acceptance/2026-10-06/final-native/03-reply-dark.png) | 真实模型回复 |
| [04-write-permission.png](../../output/acceptance/2026-10-06/final-native/04-write-permission.png) / [05-tool-success.png](../../output/acceptance/2026-10-06/final-native/05-tool-success.png) | 权限询问、工具成功 |
| [06-restored-chat.png](../../output/acceptance/2026-10-06/final-native/06-restored-chat.png) | 重启恢复 |
| [07-settings-dark.png](../../output/acceptance/2026-10-06/final-native/07-settings-dark.png) / [08-settings-light.png](../../output/acceptance/2026-10-06/final-native/08-settings-light.png) | 设置亮暗主题 |
| [09-draft-light.png](../../output/acceptance/2026-10-06/final-native/09-draft-light.png) / [10-draft-light-small.png](../../output/acceptance/2026-10-06/final-native/10-draft-light-small.png) | 亮色草稿、实际小窗口 |

## 4. 本轮发现并修复的问题

- 工作区比较：macOS `/tmp` 别名与 symlink 经 canonical 后属于相同工作区，不再误拒绝；其他工作区仍拒绝。
- 模型选择：替换模型与删除供应商后清理失效选择；保存失败保留原内存 provider/model，删除最后供应商后不能发送。
- 回合竞态：先补齐终态并发布，再清空 Running/turnID/cancel，避免调用者提前启动下一轮或清理 journal。连续30个失败回合验证终态先于空闲，相关 race 用例重复10次通过。
- 消息层级：正文与折叠按钮进入气泡子树，修复空白气泡与正文溢出。
- 编辑器：中部 Enter识别单个插入换行并提交完整原文；Shift+Enter保留换行；显式14 DIP字号及20/14行高倍数，修复 mygo placeholder 对固定行高解释错误导致的裁剪。
- UI 外壳：4 DIP inset与透明分隔槽、平台圆角、正确主区背景、空态29dvh留白与30 DIP问候、设置导航避让窗控、新建入口左对齐。
- 菜单：受窗口/320 DIP高度约束，选项可滚动且不压缩；仅高亮变化时滚入可见区，避免每帧抢回用户滚动位置。

## 5. mygo 补丁与依赖风险

本轮对外部 mygo 工作树的修改仅在 `internal/darwin/surface.go`：

1. `setFrameSize:` 从 NSView 分派 typed `objc_msgSendSuper`，避免 AppKit/KVO 动态派生类下 `SendSuper` 递归；修复前 SIGSEGV 证据在 [native-resize-crash-before.log](../../output/acceptance/2026-10-06/native-resize-crash-before.log)。
2. 有 marked text 且无 Command/Control 时，让按键交由输入法，避免编辑器抢先插入换行或破坏组合态；快捷键仍分派。

可复现补丁、应用说明和原 MIT 许可见 [patches/README.md](patches/README.md)。`git apply --reverse --check` 可确认本地改动与补丁一致。

mygo 的 Darwin 包构建与 UI 测试通过（Darwin 包无测试文件）。此前 mygo 全仓 `go test ./...` 中 terminal/PTY 插件存在 shell prompt 超时、exit129 和退出失败，**全仓没有全绿**；未确认与本补丁有关，KeenCode 首版未接入该插件，不扩展修复外部终端模块。

后续三栏工作已接入该插件，并定位Darwin master读取EAGAIN导致shell退出的问题。新增PTY补丁后terminal/PTY定向测试和原生实际命令、进程释放均通过，见 [三栏验收](workbench-2026-10-06.md)。上段保留为本报告当时的历史状态；本次没有重跑mygo全仓测试。

`go.mod` 仍使用本机绝对路径 replace，补丁未提交到依赖仓库。需锁定含修复的可下载版本并做干净 CI 构建，才能闭合发布可移植性。

## 6. 验证记录

| 命令 | 结果与日志 |
| --- | --- |
| `go test ./... -count=1` | 通过，[go-test-final.log](../../output/acceptance/2026-10-06/go-test-final.log) |
| `go test -race ./internal/app ./internal/runtime ./internal/tools -count=1` | 通过，[go-race-final.log](../../output/acceptance/2026-10-06/go-race-final.log) |
| `go test -race ./internal/runtime -run 'TestSession(JournalFailure\|Idle)' -count=10` | 通过，[runtime-terminal-race.log](../../output/acceptance/2026-10-06/runtime-terminal-race.log) |
| `go test ./internal/app -run 'TestFailedProvider\|TestStackE2E\|TestComposer' -count=1` | 最后新增保存失败测试及相关场景通过，[app-e2e-final.log](../../output/acceptance/2026-10-06/app-e2e-final.log) |
| `go vet ./...` | 通过，最终复核无输出 |
| `gofmt -l cmd internal` | 最终复核无输出 |
| `git diff --check` | KeenCode与mygo最终复核通过；未跟踪源码另行检查 |
| mygo `go test ./internal/darwin ./ui` | 通过，[mygo-targeted-test.log](../../output/acceptance/2026-10-06/mygo-targeted-test.log) |
| macOS 原生脚本 | PASS，详见上文结果与截图；原生应用由当前源码构建 |

旧 React/Tauri/Rust 栈未改动；前端 CSS 门禁不作为本 Go 原生界面的视觉验收证据。本轮结果覆盖以上具体场景，不能扩大为全产品原生矩阵或所有模型协议均已验收。

## 7. ZCode 对标结果与未完成范围

已对照固定源码修正主面板背景：`WorkspaceShellLayout.tsx:1672` 为 `bg-background`，暗色 `#161616`、亮色 `#f8f8f8`，原交接要求暗色 `#202020` 不适用于该调用点。布局、边框、圆角、空态留白和模型 trigger 本轮已继续对齐。

[ZCode 安装版外壳参考](../../output/acceptance/2026-10-06/zcode-api-setup.png) 来自隔离配置下“使用 API key”的无模型工作台。它与 Go 有模型空态状态不同，且版本并非固定源码提交产物；不能计算整屏 MAE 后声称像素复刻完成。[visual-samples.json](../../output/acceptance/2026-10-06/visual-samples.json) 的侧栏(100,300)采样落到文字，不能作为侧栏背景结论。

尚未完成：

- 固定提交 ZCode 原生重建及相同主题、项目、模型、视口、2×尺寸的基线比较。
- macOS 侧栏 vibrancy及透明层、Tahoe圆角、Windows真机；mygo支持原生Vibrancy，仍需正确处理不透明根画布。
- composer项目/权限行、模型分组/图标/响应式前缀；编辑器默认三行60 DIP与ZCode最小40 DIP仍有差异。
- 时间分段问候、20–30字号自适应、品牌与建议入口、工具/推理动效、富文本动作区和高亮等。
- 原生选择目录、会话切换/草稿/重命名/删除、拒绝/错误/取消的完整人工矩阵，以及长会话和窄窗口。
- Windows、其他IME、安装包体积、冷启动、空闲CPU与内存预算。

按 [HANDOFF.md](HANDOFF.md) 继续；优先建立匹配状态的固定视觉基线，避免以安装版不同状态截图推断复刻已完成。
