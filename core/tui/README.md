# keencode-tui

此目录仅预留 KeenCode 未来终端界面的接入边界，当前不创建 Rust crate，也不实现 TUI、终端渲染、输入循环或 Host 生命周期。

后续实现必须复用既有 ACP Client、Session 协议和 Host 所有权模型，不建立第二套 Agent Runtime 或会话状态源。引入终端 UI 依赖前，需要先给出包体、启动时间、平台兼容性和维护成本的实测依据。
