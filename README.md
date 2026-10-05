<p align="center">
  <img src="public/logo.png" width="96" height="96" alt="KeenCode logo" />
</p>

# KeenCode

一款轻量、本地优先的开源桌面 AI 编码工具。

[![CI](https://github.com/chengliang4810/keen-code/actions/workflows/ci.yml/badge.svg)](https://github.com/chengliang4810/keen-code/actions/workflows/ci.yml)
[![Release](https://github.com/chengliang4810/keen-code/actions/workflows/release.yml/badge.svg)](https://github.com/chengliang4810/keen-code/releases/latest)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

KeenCode 面向个人开发者，把项目管理、AI 编码对话、文件修改、终端命令、Diff、Git 操作和扩展管理放进一个专注的桌面工作台。应用与工作区状态均运行在本机，不依赖必须部署的配套 Web 服务。

## 主要能力

- 打开和管理本地代码项目。
- 在同一项目中创建多个对话，并让不同对话并行运行。
- 搜索、读取和修改文件，查看可审查的差异。
- 执行终端命令并保留完整过程与结果。
- 查看基础 Git 状态、Diff，并辅助提交与推送。
- 配置自定义模型供应商，不绑定单一厂商。
- 使用项目级 Goal、会话级 Todo 与单层子智能体。
- 通过插件市场、Skills 和 MCP 扩展本地工作流。
- 默认把项目、会话、配置和执行记录保存在当前设备。

## 下载与安装

从 [GitHub Releases](https://github.com/chengliang4810/keen-code/releases/latest) 下载最新版本：

- macOS Apple Silicon：选择名称中包含 `darwin` 与 `aarch64` 的 DMG。
- macOS Intel：选择名称中包含 `darwin` 与 `x64` 的 DMG。
- Windows 64 位：推荐下载名称中包含 `windows`、`x64` 与 `setup.exe` 的安装包。

当前发布范围仅包含 macOS 和 Windows。

KeenCode 启动时只读取当前更新状态；可在「帮助 → 检查更新」中主动检查 GitHub Releases。发现更新后会后台下载并验签，用户从更新入口确认安装并重启。当前源码没有 30 分钟定时检查器。

> 首次公开测试版本可能尚未配置 Apple 或 Windows 商业代码签名证书，操作系统可能显示来源提示。应用内更新签名与操作系统代码签名是两套独立校验。

## 使用前配置

首次使用时，在「设置 → 模型设置」中添加自己的模型供应商、API 地址、密钥和模型。密钥只保存在 KeenCode 本机应用配置中。

## 命令行与外部编排

除桌面应用外，KeenCode 提供非交互命令行入口 `keencode`，可在终端、CI 或脚本中运行一次性任务：

```sh
keencode run --json --no-input --cwd /path/to/repo -- "检查依赖并升级有安全问题的包"
keencode session list
```

它以 NDJSON 输出事件流，并提供稳定的退出码（`0` 成功、`1` 任务失败、`2` 参数错误、`3` Host 不可用、`4` 认证失败、`5` 取消、`6` 需要用户输入）。定时与流水线调度交给系统 cron、GitHub Actions 或 Git hook，KeenCode 本身不运行常驻调度器。`--no-input` 让模型不提问而自行决策，适合无人值守场景。

完整命令参考、事件字段、退出码契约与 cron / CI / Git hook 配方见 [docs/cli-and-external-orchestration.zh-CN.md](docs/cli-and-external-orchestration.zh-CN.md)。

## 本地开发

需要 Node.js 24、pnpm 10.14.0、Rust 1.95+（workspace MSRV；可使用更新的 stable），以及 Tauri 2 对应平台的系统构建工具。

```bash
git clone https://github.com/chengliang4810/keen-code.git
cd keen-code
corepack pnpm@10.14.0 install --frozen-lockfile
corepack pnpm@10.14.0 dev:desktop
```

常用检查：

```bash
corepack pnpm@10.14.0 typecheck
corepack pnpm@10.14.0 test
corepack pnpm@10.14.0 build
cargo test -p keencode-desktop
```

生成本机安装包：

```bash
corepack pnpm@10.14.0 build:desktop
```

## 项目结构与运行时边界

- `packages/ui/src/` 是业务 UI 根，保留 ZCode 组件的 DOM、CSS、主题令牌和 locale；`apps/ui/` 是 Vite/Tauri 平台适配、构建入口和契约测试，不维护第二套业务界面。
- `apps/desktop/` 是 Tauri Rust 宿主；Journal、资源持久化、RPC 和 Agent Runtime 由 Rust 持有权威状态，前端只消费可丢弃的投影。
- `WorkflowDefinitionV1` 是 `core/workflow/` 实现的纯 Rust `serde` JSON 契约，桌面运行时不依赖 Node、Electron 或 JavaScript workflow engine。定义、校验、存储和执行边界见 [WorkflowDefinitionV1 JSON](docs/protocols/workflow-definition-v1.md)。
- 前端来源是 ZCode 3.14.3 固定提交 `29628c9acdb81b703bbd4080c207a0e7ce5e276e`；保留来源部分按 Apache License 2.0 分发，版权主体为 Z.AI Co., Ltd.。逐文件映射、SHA-256 和许可证见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)、[SOURCE-MAPPING.md](third-party/zcode/SOURCE-MAPPING.md) 和 [third-party/zcode/LICENSE](third-party/zcode/LICENSE)。KeenCode 自有代码仍按根目录 MIT License 分发。

## 原生 WebView2 验收

原生验收是显式 opt-in 流程，不属于离线 `pnpm test` 或普通浏览器开发服务器检查。先构建带验收 feature 的桌面程序，再用隔离 provider 配置运行：

```bash
cargo build -p keencode-desktop --features native-desktop-tests
node tooling/scripts/native-live-e2e.mjs \
  --plan <plan.json> \
  --provider-config <isolated-provider-config.json> \
  --binary <keencode-desktop.exe> \
  --output <report-directory> \
  --port <free-port>
```

`--binary`、`--output`、`--port` 和 `--request-timeout-ms <1..300000>` 是可选覆盖项；脚本会把本次计划、隔离数据根、Journal 断言和前端/协议故障写入报告，并对 provider 配置中的密钥和地址做脱敏。不要把真实凭据或配置路径写入 README、计划文件或报告。验收计划和通过范围见 [前端验收矩阵](docs/frontend-acceptance-matrix.md)。

## 发布与版本

每次推送到 `main`，GitHub Actions 会先运行前端检查，再原生构建以下安装包：

- macOS Apple Silicon
- macOS Intel
- Windows x64

全部平台成功后才会公开 Release，并生成应用内更新所需的 `latest.json` 与签名产物。

对外 Release 标签采用日期与提交短哈希：

```text
vYYYYMMDD-abcdef0
```

例如：`v20260730-49ad19b`。安装包内部使用可排序的三段数字版本，以满足更新比较以及 macOS、Windows 原生版本字段要求；界面始终展示对外 Release 标签。

发布流程见 [.github/workflows/release.yml](.github/workflows/release.yml)，版本规则见 [tooling/scripts/release-version.mjs](tooling/scripts/release-version.mjs)。

## 数据与隐私

- 项目文件、会话状态、扩展配置和工具记录默认保存在本机。
- KeenCode 不提供必须经过的云端中转服务。
- 只有用户配置的模型服务、MCP Server、插件来源或任务主动访问的地址会产生网络请求。
- 项目不默认启用遥测或上传用户代码。

## Benchmark 入口

`keencode-bench` 仅供基准测试和 harness 适配器使用，不随默认桌面构建编译。构建时必须显式开启 `benchmark` feature：

```bash
cargo build --manifest-path apps/desktop/Cargo.toml --example keencode-bench --features benchmark
```

产物位于 `target/debug/examples/keencode-bench`。该入口通过标准输入读取 JSON 请求，协议定义见 [`apps/desktop/src/agent_runtime/benchmark.rs`](apps/desktop/src/agent_runtime/benchmark.rs)。

## 许可证

KeenCode 自有代码采用 [MIT License](LICENSE)。第三方依赖继续遵循各自许可证，根许可证不对第三方代码重新授权。
