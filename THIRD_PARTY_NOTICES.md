# 第三方来源与许可证声明

本文件只声明当前 KeenCode 工作树中可追溯的第三方来源，不把复制代码声明为
全部原创。根目录 `LICENSE` 继续约束 KeenCode 自有代码；下列来源部分按其
各自许可证保留。

## ZCode 3.14.3 前端基线

- 来源：ZCode 3.14.3，固定提交
  `29628c9acdb81b703bbd4080c207a0e7ce5e276e`。
- 许可证：Apache License 2.0，版权主体为 `Z.AI Co., Ltd.`。完整文本保存在
  [`third-party/zcode/LICENSE`](third-party/zcode/LICENSE)。
- 目标范围：`packages/ui/src/` 下保留的 DOM、CSS、主题令牌、组件和 locale；
  具体映射见 [`third-party/zcode/SOURCE-MAPPING.md`](third-party/zcode/SOURCE-MAPPING.md)。
- `packages/ui/src/resource-manager/` 的来源界面保留并由目标主 renderer 挂载；
  `packages/ui/src/settings/LocalAutomationsSection.tsx`、
  `packages/ui/src/browser-use/NativeBrowserView.tsx`、
  `packages/ui/src/app-shell/workflowWorkspaceEntry.ts` 是固定提交之后新增的
  KeenCode 文件，不计入 ZCode 来源归属。Rust DTO、Tauri 桥接和挂载代码同样属于
  KeenCode 适配。
- 工作流 `WorkflowRunSidePane.tsx`、`WorkflowRunSidePaneSections.tsx`、
  `WorkflowRunArtifactsSection.tsx` 与 AI Elements `sources.tsx` 均来自固定提交；
  SidePane 分区、工作区入口和 Sources CTA 仅接入 Journal/RPC 视图值，保留来源
  DOM、折叠和链接行为。逐文件 SHA-256 分类见
  [`third-party/zcode/SOURCE-MAPPING.md`](third-party/zcode/SOURCE-MAPPING.md)。
- 当前核对的 ResourceManager 10 个来源文件全部保留，其中 6 个逐文件一致、4 个
  为 Rust host/RPC 接线适配；workspace/file/workbench/group 拖拽 9 个关键文件和
  composer/preview/telemetry/AI Elements 附件 13 个关键文件均逐文件保留且与固定
  来源一致。功能清单与 SHA-256 证据见
  [`third-party/zcode/SOURCE-MAPPING.md`](third-party/zcode/SOURCE-MAPPING.md)。
- 适配范围：KeenCode 只做产品入口裁剪、品牌/语言文案和 Rust RPC 接线；
  适配不改变上述来源部分的许可证归属。
- 设计门禁例外：`third-party/zcode/design-baseline.json` 仅为固定提交中已经存在
  的原生控件、inline 布局、颜色和字号记录完整 SHA256/精确行特征。修改这些行
  或在同一目录新增代码后，门禁仍按 KeenCode 规则检查；这不是对整个 UI 目录
  的忽略。

## 组件来源清单

固定来源提交中的 `third-party/copied-components.json` 原始 SHA-256 为
`4F349A99603900B2DCB9FE529A5D93CDCCD1A80296998138E7F00407FA6330EC`。目标的
[`third-party/copied-components.json`](third-party/copied-components.json) 仅将文件图标
`roots` 映射到实际静态资源路径，原路径保留为 `sourceRoots`，不猜测原始导入版本。
当前目标清单 SHA-256 为 `3034623C5E8AFA28271BC982B7B45B2C9047CAD6BB8F45B3165C2E463C10C74F`。
目标实际保留范围、部分保留项和裁剪项见
[`third-party/zcode/SOURCE-MAPPING.md`](third-party/zcode/SOURCE-MAPPING.md)。

目标树实际使用的上游许可证文本也保存在 `third-party/upstream/`，并与 fixed
source 文件逐字节校验一致：

| 来源组件 | 许可证 | 目标许可证文本 | 实际保留范围 |
| --- | --- | --- | --- |
| shadcn/ui 上游派生部分 | MIT | `third-party/upstream/1564074e13439397221ffd522e2e504d56561994a23d371aa5e3ad43e4f5423f.txt`，SHA-256 `1564074E13439397221FFD522E2E504D56561994A23D371AA5E3AD43E4F5423F` | `packages/ui/src/components/ui/` 与 `packages/ui/src/styles.css` 中实际派生片段 |
| Vercel AI Elements 上游派生部分 | Apache-2.0 | `third-party/upstream/b4f9adb7c568904834d0dd6cc98d16c390d21ca32fc17ae7a267715269bd5529.txt`，SHA-256 `B4F9ADB7C568904834D0DD6CC98D16C390D21CA32FC17AE7A267715269BD5529` | `packages/ui/src/components/ai-elements/` 中 fixed manifest 列出的 35 个文件 |
| Visual Studio Code IPC/common utilities 派生部分 | MIT | `third-party/upstream/9480271317925265e806a9a196aaa33410a962fa9d4d1e248a4a5187bc8c9df9.txt`，SHA-256 `9480271317925265E806A9A196AAA33410A962FA9D4D1E248A4A5187BC8C9DF9` | `packages/rpc/src/` 与 `packages/shared/src/zcode-protocol-v4/wire-codec.ts` |
| Superpowers skill description 派生部分 | MIT | `third-party/upstream/a37e0e9697144819e1d965176ac4ae5bc3fa02d11e7812036bbcadf6dafe2400.txt`，SHA-256 `A37E0E9697144819E1D965176AC4AE5BC3FA02D11E7812036BBCADF6DAFE2400` | `packages/ui/src/lib/builtinSkillI18n.ts` |
| Material Icon Theme 文件图标 | MIT | `third-party/upstream/cdab3014d4f69b49dde2b85e81792208c72de613aa6aed7f7a9b5c6609b89670.txt`，SHA-256 `CDAB3014D4F69B49DDE2B85E81792208C72DE613AA6AED7F7A9B5C6609B89670` | `apps/ui/public/material-icons/` 中 1146 个 SVG；版权主体 Material Extensions |

同一目录中的 KeenCode/ZCode 适配文件不自动获得上游归属，必须以文件内容和
fixed manifest 为准。固定来源中没有当前目标路径的 Fig autocomplete、
agent-browser skills、React Best Practices 条目只在 SOURCE-MAPPING
中记录为裁剪项，没有被伪装成已复制来源。

浏览器源与构建产物不包含 Electron/Node 静态宿主导入或 TypeScript Agent 执行器；
`zcodeAgent.ts` 是 Rust host service descriptor，`packages/rpc/examples/` 的 Node
demo 已裁剪。Electron 兼容类型、历史注释及第三方库的 guarded browser/file URL
环境分支不构成宿主入口，来源审计报告记录了这些非运行时文本信号。

## Tao Windows 输入重入修复

Rust 桌面宿主使用 Tao（Apache-2.0），本次通过 Cargo patch 固定官方修复提交
`c704261c519c58cfdd0bc2d58ba24e06a0b71c92`（crate 版本 0.35.3），解决 Windows
键盘／IME 消息重入导致的主线程死锁。没有复制或改写窗口实现；保留许可证和精确来源见
[`third-party/tao/SOURCE.md`](third-party/tao/SOURCE.md) 与
[`third-party/tao/LICENSE`](third-party/tao/LICENSE)。关联 `tao-macros` 为同源 0.1.3；
当前 Windows 原生验收不涵盖该提交的 Linux JIS 映射变更。

## 产品裁剪与历史

官方账号、支付、云端、机器人、SSH、WSL、Docker 和浏览器计算机控制不在本次
目标 UI 范围内。被裁剪内容不会因为仍存在于来源仓库就成为 KeenCode 的运行
依赖。旧来源说明文件如仍出现在工作树，只能作为待清理历史，不是当前前端的
权威来源；当前权威来源是本文件、`third-party/zcode/` 和 `DESIGN.md`。

新增或修改第三方代码时，必须同步记录固定提交/版本、许可证、目标路径、是否
修改以及删除或裁剪范围，并保留原始版权声明。
