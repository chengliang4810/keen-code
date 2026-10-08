# RCode

[简体中文](README.md) | [English](README.en.md)

<img src="apps/desktop/public/logo.png" width="96" height="96" alt="RCode" />

RCode 是围绕项目和独立任务组织的 Agent 开发工作台。R 代表 Rust 与 Result。

对话是主要工作区，文件、Git、终端、编辑器和预览位于开发工具面板。

应用位于 `apps/`，共享 Rust 核心位于 `crates/`，共享前端包位于 `packages/`。Desktop 与独立 Agent CLI 已有运行入口，TUI 和 Web 预留职责约定，见 [多应用工作区](docs/workspace.md)。

## 功能

- 项目与任务管理，保存对话和草稿。
- 原生 Rust Agent，支持 Chat Completions、Responses、Messages，以及流式输出、推理、工具调用和审批。
- 文件浏览、代码编辑、Git 变更与历史、终端和网页预览。
- 自定义角色、只读子智能体、MCP 服务器、技能、插件和命令。
- 工作区记忆和文件式智能体指令。
- 自定义模型供应商和本地模型。

## 开发

需要 Node.js 22+、pnpm、Rust 1.95+，以及 [Tauri 平台依赖](https://tauri.app/start/prerequisites/)。

```sh
pnpm install --frozen-lockfile
pnpm tauri dev
```

## 检查

```sh
pnpm lint
pnpm check-types
pnpm test
pnpm build
pnpm knip
pnpm size
```

Knip、构建体积检查和 Nix 分发配置集中在 `config/`，用途和运行方式见 [配置说明](config/README.md)。

在仓库根目录运行 Rust workspace 检查：

```sh
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## 构建

```sh
pnpm tauri build
```

## 用户文件

RCode 将设置、对话、角色、命令、技能和工作区记忆保存在 `~/.rcode`。

智能体指令由当前角色、`~/.rcode/AGENTS.md` 和项目根目录 `AGENTS.md` 组成。项目技能从 `.agents/skills` 发现。

## 许可证

RCode 代码采用 MIT 许可证，参见 [LICENSE](LICENSE)。

本项目基于 [Terax](https://github.com/crynta/terax-ai) 二次开发并做了大量修改。Terax 版权归 Crynta 所有（Copyright 2026 Crynta），采用 Apache License 2.0，其许可文本保留于 [LICENSES/Terax-Apache-2.0.txt](LICENSES/Terax-Apache-2.0.txt)。第三方组件声明见 [NOTICE](NOTICE) 与 [LICENSES/](LICENSES)。
