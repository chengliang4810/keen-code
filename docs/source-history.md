# 来源历史与退役注入审计

本文件是 clean-room 来源历史 allowlist 中的专门记录。它保留真实退役来源名称、
路径和证据范围，供验收矩阵、运行时 provenance 和门禁复核引用；普通验收文档只
链接本文件，不把来源历史词面混入产品验收结论。

## 退役 guest 注入记录

根 Agent 已删除 3 个实际旧 `Synara` guest 注入文件。该删除不能被描述为“旧 guest
无害”，也不能把备份中的历史内容当作当前产品文件。完整变更快照位于
`D:/projects/keen-code-backups/zcode-replacement-20261002-215853`；运行时来源审计
见 `docs/frontend-source-runtime-provenance-20261003.md:58`，其中记录了此前自动注入的
`apps/desktop/src/browser/annotation-guest.generated.js`、专用 annotation CDP/接线
注册的删除范围。

当前源码中仍可见的 `Synara` 只落在终端测试环境变量和 `synara/<token>` worktree
历史/测试分支规则中。这些残余标识不能等同于仍有 guest 注入文件，也不能据此声称
所有历史标识已清零。备份中可追溯的来源目录包括
`third-party/synara/browser-annotation-guest/` 及其许可证/补丁记录；它们仅用于
来源取证和删除范围复核，不构成目标产品运行时依赖。

## 范围区分

普通浏览器工具栏、tab 和受管 child WebView 属于目标 retained 能力；它们的
`NativeBrowserView`/`UnifiedBrowserView` 命名来自来源视图适配，不能按关键字推断为
已退役的 OS ComputerUse。ComputerUse 组件/Composer 入口的当前删除状态应由产品
验收矩阵和源码扫描记录；官方控制插件 card/skills 初始化残留另行列为待裁剪项。

## native26 前端清理核验

`out/native-live/source-provenance-runtime-20261003.json` 记录真实扫描：浏览器运行时
源码范围共 1641 个文件，`Synara` 标识为 0；native26 前端产物共 3633 个文件，
`Synara`、TSAgent/`typescript-agent` 和 `ELECTRON_RUN_AS_NODE` 均为 0。
这里的结论仅限前端源码与产物，不能扩大到上文仍有历史标识的 Rust 测试或备份。
产物聚合 SHA-256 为 `8FCBB4A8F6858C1B185A54758B28B58BCF26F048EF914DDAF980E158345E2CD8`。
