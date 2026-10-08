# KeenCode

KeenCode 是面向个人开发者的纯 Rust AI 编程工作台。GPUI 负责原生窗口和绘制，Rust
Host 持有项目、会话、Agent、工具、Journal、资源和工作流的权威状态。

## 主要能力

- 打开和管理本地代码项目、会话、工作树和分组。
- 与用户配置的模型供应商进行流式对话，执行受控的文件、终端和 Git 操作。
- 查看消息、工具结果、Diff、历史、资源、Memory、Goal、Automation 和工作流状态。
- 使用 Skills、MCP、插件和单层子 Agent 扩展本地工作流。
- 在本机保存项目、会话、配置、草稿和执行记录；敏感值通过系统密钥存储保护。
- 通过 `keencode` 提供非交互命令行入口，适合终端、CI 和外部调度器。

## 使用前配置

首次启动后，在原生设置页添加模型供应商、API 地址、密钥和模型。密钥只保存在本机，
应用不会替用户配置云端中转服务。

## 本地开发

需要 Rust 1.95 或更新的 stable，以及目标平台的标准 Rust 构建工具。克隆后直接使用
Cargo：

```bash
git clone https://github.com/chengliang4810/keen-code.git
cd keen-code
cargo run -p keencode-desktop
```

常用检查：

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check advisories bans licenses sources
```

构建发布版：

```bash
cargo build --release --workspace
```

## 项目结构与运行时边界

- `core/` 包含 ACP、Agent、Model、Provider、MCP、资源、Runtime、Tools 和 Workflow 等
  可复用 Rust crate。
- `apps/desktop/src/native_ui/` 是 GPUI 原生界面；`apps/desktop/src/native_host/` 负责
  窗口内宿主、投影、事件和动作装配。
- `NativeHost` 是界面与领域服务的类型化边界。界面保存短生命周期的投影和草稿，Journal
  与 Rust 服务保存事实。
- `WorkflowDefinitionV1` 是 `core/workflow/` 提供的 Rust `serde` JSON 契约，定义、校验、
  存储和执行边界见 [WorkflowDefinitionV1 JSON](docs/protocols/workflow-definition-v1.md)。
- ZCode 3.14.3 固定提交
  `29628c9acdb81b703bbd4080c207a0e7ce5e276e` 作为布局、样式和交互的固定验收基线；GPUI 与 Ely 的固定
  来源、许可证和当前边界见 [第三方声明](THIRD_PARTY_NOTICES.md) 与
  [ZCode 设计参考](docs/frontend-zcode-source.md)。

## 原生验收

Windows 原生验收器通过真实桌面窗口、输入、截图和可访问性树验证 GPUI 应用。先构建
release Host 和验收器：

```bash
cargo build -p keencode-desktop --release
cargo build -p keencode-native-gpui-tests
```

再用隔离数据根和显式计划运行：

```bash
cargo run -p keencode-native-gpui-tests -- --binary target/release/keencode-desktop.exe --plan tooling/native-gpui-tests/ely-native-smoke.json --output out/ely-native/run-current
```

需要真实模型的计划还必须显式提供隔离 Provider 配置和 `--real-provider`。不要把凭据、
配置值或未脱敏报告写入仓库。没有当前窗口报告的范围保持 `pending`。

## 命令行与外部编排

```sh
keencode run --json --no-input --cwd /path/to/repo -- "检查依赖并升级有安全问题的包"
keencode session list
```

CLI 输出 NDJSON 事件并提供稳定退出码；定时和流水线调度交给系统 cron、GitHub Actions
或 Git hook。命令、字段、退出码和编排示例见
[CLI 与外部编排](docs/cli-and-external-orchestration.zh-CN.md)。

## 发布产物

`.github/workflows/release.yml` 在 `main` 推送或手工触发时运行完整 Rust workspace 检查，
按目标平台构建桌面应用、CLI 和验收工具，并为每个可执行文件生成 SHA-256 清单。产物作为
GitHub Actions artifact 保存，不创建外部 Release，不生成安装更新清单，也不使用签名密钥。

## 数据与隐私

- 项目文件、会话状态、配置、扩展和工具记录默认保存在本机。
- 只有用户配置的模型服务、MCP Server、插件来源或任务主动访问的地址会产生网络请求。
- 项目默认不启用遥测，也不会主动上传用户代码。

## 许可证

KeenCode 自有代码采用 [MIT License](LICENSE)。第三方依赖继续遵循各自许可证，根许可证
不对第三方代码重新授权。
