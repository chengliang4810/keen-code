# 第三方来源与许可证声明

本文件记录当前 Rust 产品和设计参考中可追溯的第三方来源。根目录 `LICENSE` 只约束
KeenCode 自有代码；第三方依赖和参考材料继续遵循各自的许可证。

## GPUI

- 来源仓库：<https://github.com/zed-industries/zed>
- 固定提交：`1a28cff4b409169bac058bca40dfbfeb7621d19b`
- 使用 crate：`gpui`、`gpui_platform` 及其固定提交中的平台支持 crate。
- 许可证：Apache-2.0。GPUI 源码的 `LICENSE-APACHE` 和版权声明随固定来源仓库提供。
- 用途：原生窗口、布局、绘制、输入、文本和平台事件；当前代码通过 Cargo git 依赖使用，
  没有把上游界面源码复制到 KeenCode。

## Ely GPUI Components

- 来源仓库：<https://github.com/ZacharyZhang-NY/Ely-GPUI-Components>
- 固定提交：`94f34c9f8e98b5f4b3078776a4c197b9021f4cdf`
- 使用 crate：`ely-gpui-component`。
- 许可证：MIT；本次固定提交的上游许可证文件为 `LICENSE`。
- 用途：主题、按钮、输入、设置行、图标和其他 GPUI 原生控件。应用代码只通过公开的
  Rust API 使用这些控件，不把其示例站点或展示资源当作产品运行时。
- 本地副本：`third-party/ely-gpui-component/` 保留编译源码、静态资产和许可证，
  通过 Cargo path 依赖使用。局部适配覆盖按钮和下拉触发器密度、`InputStyle` 与单行
  `TextAlign`、菜单/选项/Switch/快捷键 UIA 语义、代码主题设置以及编辑器软换行和
  shaping 缓存；完整复制边界、当前文件清单、差异说明和归一化 SHA-256 见
  [`SOURCE.md`](third-party/ely-gpui-component/SOURCE.md)。来源说明记录的是可追溯的
  vendor 副本，不声明其中全部组件都已在 KeenCode 生产 UI 中启用。

## Lucide icons

- 来源仓库：<https://github.com/lucide-icons/lucide>；来源包为固定 ZCode 3.14.3 提交中
  的 `lucide-react` v1.17.0。
- 许可证：ISC；许可证全文和应用 SVG 到来源文件的映射见
  [`third-party/lucide/LICENSE`](third-party/lucide/LICENSE) 与
  [`third-party/lucide/SOURCE-MAPPING.md`](third-party/lucide/SOURCE-MAPPING.md)。
- 用途：补齐应用静态资源 `message-circle-plus`、`calendar-clock`、`blocks`、`settings-2`、
  `brain`、`cable`、`anchor`、`list-filter`、`square-terminal` 和 `panel-right-open`；运行时
  通过 `NativeAssets` 覆盖加载，其余图标继续委托 Ely 资源源。

## ZCode 设计参考

- 来源：ZCode 3.14.3，固定提交
  `29628c9acdb81b703bbd4080c207a0e7ce5e276e`。
- 许可证：Apache License 2.0，版权主体为 `Z.AI Co., Ltd.`。来源许可证全文保存在
  [`third-party/zcode/LICENSE`](third-party/zcode/LICENSE)。
- 用途：整体 UI 仅用于研究并记录布局密度、主题层级、键盘交互和信息组织，参考 UI 源码
  未复制为运行时组件。`window-maximize.svg` 和 `window-restore.svg` 是从固定来源
  `packages/ui/src/components/icons/windowIcons.ts` 转换得到并在 runtime 使用的静态资源；
  Apache-2.0 与 `Z.AI Co., Ltd.` 版权归属继续适用于该来源。
- 映射：参考到原生界面的说明见
  [`docs/frontend-zcode-source.md`](docs/frontend-zcode-source.md) 和
  [`third-party/zcode/SOURCE-MAPPING.md`](third-party/zcode/SOURCE-MAPPING.md)。

## 历史来源记录

`third-party/upstream/` 中的许可证快照和 `third-party/zcode/` 中的来源材料用于保持历史
可追溯性。它们不表示当前产品复制了对应组件；当前 Ely 本地副本和明确登记的静态资源派生项
记录在复制组件清单 [`third-party/copied-components.json`](third-party/copied-components.json) 中。

Cargo 依赖的 registry 和 Git 来源由 `deny.toml` 审计。Git 来源必须使用固定 revision；
新增第三方 crate 时，应同步记录仓库、revision、许可证、用途和是否包含本地修改，并保留
上游版权声明。
