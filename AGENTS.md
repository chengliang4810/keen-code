# KeenCode Agent 开发指南

本文是仓库级开发约束。用中文交流，先从当前源码、配置、测试和 Git 状态确认事实；
保留与任务无关的本地修改，不重置、覆盖或提交其他 Agent 的工作。

## 当前基线

- 根 `Cargo.toml` 管理唯一的 Rust workspace。业务核心位于 `core/`，桌面应用位于
  `apps/desktop/`，Windows 原生验收器位于 `tooling/native-gpui-tests/`。
- 桌面界面由 GPUI 原生绘制，界面实现位于 `apps/desktop/src/native_ui/`。GPUI 固定在
  `1a28cff4b409169bac058bca40dfbfeb7621d19b`，Ely GPUI Components 固定在
  `94f34c9f8e98b5f4b3078776a4c197b9021f4cdf`；不要混用未锁定的版本。
- ZCode 3.14.3 提交
  `29628c9acdb81b703bbd4080c207a0e7ce5e276e` 定义固定像素基线，约束视觉、密度、交互和
  信息层级。当前产品不复制其界面源码；来源事实和 Apache-2.0 归属见
  `THIRD_PARTY_NOTICES.md`、`docs/frontend-zcode-source.md` 与 `third-party/zcode/`。
- `NativeHost`、Agent Loop、Journal、资源持久化和工作流执行器由 Rust 持有权威状态；
  GPUI 组件只保存当前投影、输入草稿和短生命周期的交互状态。
- workspace MSRV 是 Rust 1.95；开发和发布均使用 Cargo，不需要额外的脚本运行时。

## 开始与编辑

1. 先执行 `git status --short`，再用 `rg` 或 `rg --files` 定位目标文件、调用方和相邻
   测试。不要通读无关的大文件或生成输出。
2. 修改界面前阅读 `DESIGN.md`、`docs/frontend-zcode-source.md` 和相邻的
   `apps/desktop/src/native_ui/` 实现；修改 Journal、快照或工作流前阅读
   `docs/protocols/` 下对应契约。
3. 优先复用 `ely-gpui-component` 和现有 GPUI 组件，保持现有命名、焦点语义、键盘行为、
   主题角色和信息密度。业务状态必须通过类型化 Rust 服务或宿主回执更新。
4. 新增或修改的非直观语义、边界和取舍使用简短中文注释；JSON 等格式不写非法注释。
   不为门禁引入伪造包装层或重复的状态源。
5. 只有用户明确要求时才提交或推送；当前迁移阶段不要创建提交。

## 原生界面规则

- 颜色来自 Ely `Theme` 的语义调色板，例如 `bg`、`surface`、`sunken`、`border`、
  `fg`、`fg_muted`、`accent`、`success`、`warning` 和 `danger`。界面代码不写无语义的
  固定颜色来替代主题角色。
- 文字使用 `Theme::text_size(TextSize::...)`，代码、路径、命令、标识符和终端内容使用
  主题提供的等宽字体。不要为单个控件另造字号体系。
- 侧栏、聊天、输入区、设置和工作台保持紧凑、可扫描、可键盘操作的桌面工作台结构。
  不用装饰性大块面板、无意义的渐变或重复的浮层包裹信息。
- 每个有状态的动作都应有明确的加载、错误、禁用和回执状态。失败时显示脱敏诊断，
  不能用本地乐观状态冒充 Journal 已确认的结果。
- 官方账号、支付、云端消息、机器人、SSH、WSL、Docker 和计算机控制入口不属于目标
  产品；不要通过菜单、设置、资源或文案重新引入这些范围。

## 协议与状态

- Journal 是会话、消息、工具和工作流执行事实源；快照只用于恢复投影。顺序缺口、重复
  事件或宿主重置时，丢弃受影响的局部投影并重新读取权威状态。
- active barrier 由 Rust host 建立和解除，界面不得凭按钮状态宣布执行完成。成功、取消、
  失败和需要用户输入都必须来自宿主事实或回执。
- `WorkflowDefinitionV1` 是 Rust `serde` 负责校验和持久化的纯 JSON 数据。ID、版本、
  引用和环由 Rust 检查；草稿变化不能改变正在执行的 revision。
- 草稿、侧栏分组和本地缓存使用有界大小、原子写入和跨进程锁；冷恢复必须绕过可能过期
  的进程内缓存。敏感配置只通过既有密钥存储和脱敏诊断流转。

## 验证

从仓库根按改动范围运行：

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check advisories bans licenses sources
cargo build -p keencode-desktop --release
cargo build -p keencode-native-gpui-tests
```

Windows 原生验收器只在有显式计划和隔离 Provider 配置时运行：

```text
cargo run -p keencode-native-gpui-tests -- --binary target/release/keencode-desktop.exe --plan tooling/native-gpui-tests/ely-native-smoke.json --output out/ely-native/run-current
```

真实 Provider、性能和人工窗口操作是 opt-in 验收。没有当前命令、环境和报告证据的项目
保持 `pending`，不能把源码存在或离线单测通过写成完整功能验收通过。

完成前检查 `git diff` 与 `git diff --check`，报告实际验证结果和未验证范围。
