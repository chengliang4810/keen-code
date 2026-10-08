# TUI 应用

此目录预留终端交互应用，当前没有可运行入口。

实现时使用独立 Rust crate，并依赖 `crates/rcode-runtime` 和现有核心库。界面输入、渲染与终端生命周期归本应用，Agent Loop、工作区路径规则、审批与取消语义归共享库。不得依赖 Desktop 或复制桌面宿主的业务规则。

工作区约定见 [多应用工作区](../../docs/workspace.md)。
