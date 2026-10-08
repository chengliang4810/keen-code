# ZCode 设计参考映射

## 固定来源

- 项目：ZCode 3.14.3
- revision：`29628c9acdb81b703bbd4080c207a0e7ce5e276e`
- 许可证：Apache License 2.0
- 版权主体：`Z.AI Co., Ltd.`
- 许可证全文：[`LICENSE`](LICENSE)

该来源用于记录 KeenCode 的视觉、密度、信息组织和交互参考。ZCode 参考 UI 源码未复制为
运行时组件，也没有按来源目录自动豁免新的代码。窗口控制图标是明确登记的静态资源派生项：
`window-maximize.svg` 和 `window-restore.svg` 从固定来源
`packages/ui/src/components/icons/windowIcons.ts` 转换得到，并在 runtime 使用，详见下表。

## 原生实现映射

| 参考主题 | KeenCode Rust/GPUI 位置 | 适配说明 |
| --- | --- | --- |
| 工作台结构 | `apps/desktop/src/native_ui/main_workbench.rs` | 由 GPUI 根实体组合页面和面板 |
| 项目与会话导航 | `apps/desktop/src/native_ui/sidebar.rs` | 显示 Rust 宿主返回的项目、会话和分组事实 |
| 对话与工具输出 | `apps/desktop/src/native_ui/chat.rs` | 使用 Journal 归约后的有界消息投影 |
| 输入区 | `apps/desktop/src/native_ui/composer.rs` | 直接调用类型化宿主 API，草稿由 Rust 持久化 |
| 设置 | `apps/desktop/src/native_ui/settings/` | 使用 Ely 主题和设置组件 |
| 工作台面板 | `apps/desktop/src/native_ui/workbench/` | 文件、终端、Diff、Git 和工作树的原生实现 |
| 窗口控制图标 | `apps/desktop/assets/icons/window-maximize.svg`、`apps/desktop/assets/icons/window-restore.svg` | 由 `packages/ui/src/components/icons/windowIcons.ts` 的 `WindowMaximizeIcon` 和 `WindowRestoreIcon` 路径转换为 GPUI 静态 SVG；保留 24x24 viewBox、currentColor 和全局线宽 |

窗口图标来源文件 `packages/ui/src/components/icons/windowIcons.ts`（固定来源 SHA-256
`aab0063f8495f809b833fac705dbe995cd9abfbd1f8105a7a915235068635fb1`）采用 Apache
License 2.0。应用资源 SHA-256 为：`window-maximize.svg`
`6f374ce8f9d241d11b5337e36c212b8150a14ab8bd5a69b478fd4efef1fe5770`，
`window-restore.svg`
`d41459f9ab347256bc009a08528b02856a75a69e92121f4cc58c2fdd29aa6fad`。

这些路径是从固定来源转换后纳入 KeenCode 的静态运行时资源，不是完整的 ZCode 参考 UI
组件；Apache-2.0 与 `Z.AI Co., Ltd.` 版权归属仍适用于来源路径。ZCode 只决定需要对齐的
体验目标；实际状态、生命周期、权限、错误和持久化由 Rust 契约决定。

## 适配规则

1. 使用 `ely-gpui-component` 和 GPUI 的语义主题令牌，不从参考材料引入独立的颜色、字号
   或布局系统。
2. 使用稳定的项目 ID、会话 ID、Journal sequence、revision 和 operation ID；不以标题、
   列表下标或点击时间作为事实标识。
3. 流式消息、草稿、分组和工作树状态都必须有界；事件缺口或宿主重置时重新读取事实。
4. 未经原生窗口报告验证的交互范围保持 `pending`，不能因为设计映射存在而标记完成。

## 相关记录

- 设计边界：[`docs/frontend-zcode-source.md`](../../docs/frontend-zcode-source.md)
- 许可证与完整第三方归属：[`THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md)
- 全局复制组件清单：[`third-party/copied-components.json`](../copied-components.json)，
  当前包含 Ely GPUI Components、Lucide 静态图标和 ZCode 窗口 SVG 派生资源；ZCode 参考 UI
  源码本身不计入运行时复制清单。
