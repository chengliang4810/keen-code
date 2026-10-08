# 多应用工作区

仓库根目录管理 Cargo workspace、pnpm workspace 和开发工具。可运行应用位于 `apps/`，共享 Rust 库位于 `crates/`，共享 TypeScript 和 WASM 包位于 `packages/`。

```text
apps/
  cli/                 独立 Agent 命令与桌面控制命令
  desktop/
    src/               Tauri 原生宿主与桌面集成
    ui/                React 界面与桌面客户端
  tui/                 终端交互应用的职责约定
  web/                 浏览器应用的职责约定
crates/
  rcode-runtime/       运行装配、共享业务工具、租约、审批、取消、事件与安全边界
  rcode-agent/         Provider 中立 Agent Loop
  rcode-model/         消息与模型协议
  rcode-provider/      模型协议适配
  rcode-tools/         工具实现
  rcode-mcp/           MCP 客户端
  rcode-skills/        Skills 发现与加载
  rcode-plugins/       插件管理
  rcode-control-protocol/ 桌面控制协议与用户根目录约定
packages/
  ghostty-core/        共享终端模型与 WASM
```

## 依赖方向

应用依赖共享库；共享库不依赖应用、Tauri 或 React。`rcode-runtime` 接收宿主的事件投递回调，Desktop 将事件交给 Tauri Channel，CLI 将同一事件输出为 NDJSON 或文本。

Desktop 和 CLI 使用同一组本地工具、Todo、只读子 Agent、后台 Shell、工作区路径策略和权限规则。内置子 Agent 模板也集中在共享 Rust 资源中，桌面仅处理配置和展示。WSL 保留现有 AI SDK 主循环，通过薄 IPC 适配使用同一业务工具和 Rust 审批门。内置 Google 模型接入已移除。`ask` 请求变更审批，`edit` 自动批准文件变更，`full-access` 自动批准状态变更；所有模式保留敏感路径限制。Plan 模式不注册变更工具，Agent Loop 同时执行只读守卫。运行租约限制并发并禁止同一会话同时运行两个任务，结束后释放审批与客户端工具等待。

用户私有路径由 `crates/rcode-runtime/src/storage.rs` 定义，桌面存储命令继续提供现有前端投影。CLI 与 Desktop 各自持有运行实例，不做跨端同步或接手。CLI 单次运行不写入桌面对话历史；Desktop 的会话持久化、界面工具、Skills、MCP 和插件接线仍由桌面宿主提供。

## 开发与验证

从仓库根目录运行命令：

```sh
pnpm install --frozen-lockfile
pnpm dev
pnpm tauri dev
pnpm lint
pnpm check-types
pnpm test
pnpm build
pnpm size
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

根目录 pnpm 命令转发到 `@rcode/desktop`，该应用持有前端依赖、Vite、TypeScript、组件生成配置和资源。通用检查与分析工具保留在根目录。桌面前端产物位于 `apps/desktop/dist/`，Cargo 产物位于根目录 `target/`，CLI sidecar 构建脚本将指定目标的二进制复制到 `apps/desktop/binaries/`。

## 独立 CLI

CLI `run` 使用共享 Runtime，不需要启动 Desktop。显式指定服务地址和模型，模型密钥从 `RCODE_API_KEY` 环境变量读取，不接受命令行明文密钥。

```sh
pnpm cli run --base-url http://127.0.0.1:1234/v1 --model local-model --plan "分析当前项目"
pnpm cli run --base-url http://127.0.0.1:1234/v1 --model local-model --permission edit "修改当前工作区内的文件"
pnpm cli run --base-url http://127.0.0.1:1234/v1 --model local-model --json "检查当前项目"
```

`--protocol` 支持 `chat-completions`、`responses`、`messages`。`--cwd` 固定工具的工作目录。指令按 CLI 默认角色、`~/.rcode/AGENTS.md`、工作区根目录 `AGENTS.md` 顺序装配，合计上限为 256 KiB。

默认权限是 `ask`。终端中请求用户审批，非交互输入直接拒绝需要审批的操作；自动化可显式选择 `edit` 或 `full-access`。Ctrl+C 取消运行并通知工具清理进程；CLI 退出会等待后台进程树回收。`--json` 将运行事件逐行输出到 stdout，命令错误输出到 stderr。原有 `open`、`ping`、`capabilities`、`identify` 命令继续通过桌面控制协议工作。

## 后续应用

TUI 和 Web 当前只有职责说明，没有可运行入口或空依赖清单。TUI 实现时组合已有 Runtime；Web 实现时需要无界面 Rust 服务。Desktop 当前在 Tauri 进程中调用共享 Runtime。迁往独立服务进程时，应先完成服务启动、退出、鉴权、事件通信、取消和审批的端到端验证，再切换桌面调用入口。

只有出现两个前端的实际复用需求时才抽取 `packages/ui` 或客户端包。共享前端包不得直接访问 Tauri；桌面特有能力由 Desktop 接入。
