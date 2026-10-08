# RCode Agent 开发指南

本文件规定在本仓库开发时的工作边界、源码入口和验证要求。产品内 Agent 的权限、Plan 和子 Agent 机制不构成开发仓库的额外授权。详细模块说明见 [开发架构参考](docs/development-reference.md)，终端实现与诊断见 [终端专项文档](docs/terminal.md)。只读取本次任务涉及的参考章节。

## 协作与修改边界

- 用中文沟通，结论先行，说明结果、验证和未验证范围，省略非必要过程说明。
- 根据请求区分调查、评审、计划和实现。调查或评审不自动授权修改；未受阻的模糊任务采用合理假设继续。
- 开始前检查 `git status --short`，保留无关改动。没有用户授权，不覆盖、删除本地文件，不提交或推送。
- 搜索优先使用 `rg` / `rg --files`。先读目标模块、调用方、相邻测试和依赖清单，再修改。
- 以当前源码、`package.json`、`apps/desktop/package.json`、根目录 `Cargo.toml`、`apps/desktop/Cargo.toml` 和配置为准；文档有差异时核实并同步修正。
- 在共同原因处做最小完整修改，职责清晰，不顺手重构，不新增缺少实际需求的抽象、配置或依赖。
- 先评估已有依赖与成熟方案，查阅文档和类型后再决定是否自行实现。新增依赖说明必要性及体积、启动、内存和维护影响。
- 不以向后兼容为目标。已废弃的字段、代码路径和接口直接移除，不增加历史路径、旧数据迁移或废弃 API 包装。
- 仅在用户要求时提交或推送。按功能点分别提交，提交信息使用中英双语，只暂存本次相关路径或片段，不擅自 amend、重写历史或绕过 hooks。

## 当前项目与开发命令

RCode 是围绕项目和独立任务组织的本地 Agent 开发工作台。中央为会话，右侧提供文件、Git、终端、编辑器和预览。当前使用 React 19、TypeScript、Vite、Tailwind v4、Tauri 2、Rust、portable-pty 和 libghostty-vt WASM，源码包含 macOS、Linux、Windows 支持。应用标识为 `app.rcode.workbench`。

- 使用 `package.json` 固定的 pnpm，当前为 `11.9.0`；Node.js 至少为 22，Rust 至少为 1.95，并安装所在平台的 Tauri 构建依赖。
- 开发仓库仅使用 pnpm，不使用 npm、npx 或 yarn。
- Rust workspace 位于根目录 `Cargo.toml`，锁文件为根目录 `Cargo.lock`；从仓库根目录执行 Cargo 命令。

```sh
pnpm install --frozen-lockfile
pnpm tauri dev
```

`pnpm dev` 只启动前端。`pnpm tauri dev` 先构建开发 CLI sidecar，再启动前端和原生宿主；浏览器验收不能代替原生桌面验收。

## 源码入口

| 范围 | 优先检查 |
| --- | --- |
| 启动、界面装配、开发工具布局 | `apps/desktop/ui/main.tsx`、`apps/desktop/ui/app/` |
| 会话状态、历史和 Chat 实例 | `apps/desktop/ui/modules/ai/store/chatStore.ts`、`apps/desktop/ui/modules/ai/store/chatRuntime.ts`、`apps/desktop/ui/modules/ai/lib/sessions.ts` |
| 协议选择、运行传输和消息投影 | `apps/desktop/ui/modules/ai/lib/transport.ts`、`apps/desktop/ui/modules/ai/lib/nativeProtocol.ts`、`apps/desktop/ui/modules/ai/lib/nativeTransport.ts` |
| 共享 Agent Runtime、审批和运行控制 | `crates/rcode-runtime/` |
| 桌面 Agent IPC、指令和扩展接入 | `apps/desktop/src/modules/agent_core/` |
| Provider 中立模型、协议适配、Agent Loop 和工具 | `crates/rcode-agent/`、`crates/rcode-provider/`、`crates/rcode-tools/` |
| Tauri 命令、文件、Git、进程和系统能力 | `apps/desktop/src/lib.rs`、`apps/desktop/src/modules/` |
| 终端模型、渲染和 PTY | `apps/desktop/ui/modules/terminal/`、`packages/ghostty-core/`、`apps/desktop/src/modules/pty/`；先读终端专项文档 |
| 编辑器、LSP、文件树和标签页 | `apps/desktop/ui/modules/editor/`、`apps/desktop/ui/modules/lsp/`、`apps/desktop/ui/modules/explorer/`、`apps/desktop/ui/modules/tabs/` |
| 设置、主题、翻译和通用控件 | `apps/desktop/ui/settings/`、`apps/desktop/ui/modules/settings/`、`apps/desktop/ui/modules/theme/`、`apps/desktop/ui/modules/i18n/`、`apps/desktop/ui/components/ui/` |
| 样式与组件生成配置 | `apps/desktop/ui/styles/globals.css`、`apps/desktop/components.json` |
| 固定存储路径、密钥和界面投影 | `crates/rcode-runtime/src/storage.rs`、`apps/desktop/src/modules/secrets.rs`、`apps/desktop/ui/lib/storage.ts`、`apps/desktop/ui/lib/uiState.ts` |
| CLI、打包、资源和预算检查 | `scripts/`、`apps/cli/`、`apps/desktop/tauri.conf.json`、`config/` |

## 架构与状态约束

- 共享 Rust Runtime 与工具库不得依赖 Tauri 或前端。CLI 可以独立运行 Agent；Desktop 负责桌面 IPC、系统集成、界面工具与扩展接线。Rust 持有文件系统、进程、PTY、Git、密钥和网络代理能力；Webview 经注册的 Tauri 命令访问。所有外部输入、IPC、路径和网络边界均须校验，读写都不得绕过工作区和敏感路径限制。
- `apps/desktop/ui/app/App.tsx` 只负责装配与跨域协调。业务逻辑放对应模块，纯规则保持依赖少、可测试，Tauri 命令和 React 组件保持轻量。
- Agent core、工具和会话语义保持 Provider 中立。当前本地 Chat Completions、Responses、Messages 使用 Rust `rcode-agent::AgentRunner`；Google 和 WSL 使用现有 AI SDK v6 路径，不把两条路径的能力混为一谈。
- 修改事件或数据契约时同时检查 Rust 定义、桌面转发、前端解码与投影、持久化恢复和测试。
- 产品内主会话指令依次为当前角色的完整指令、`~/.rcode/AGENTS.md`、工作区根目录 `AGENTS.md`。全局指令通过文件编辑，不保存到偏好设置。
- 任务目录由 `getTaskWorkspace(sessionId)` 确定，文件工具和原生请求使用该目录。切换开发工具标签页不能改写任务根；终端内容仅在需要时读取。
- 会话元数据和消息经 `@/lib/storage` 保存到 `~/.rcode/sessions/conversations.json`。`useAiBootstrap` 触发初始化，历史消息按需加载；`chatRuntime.ts` 的 `getOrCreateChat(sessionId)` 复用有界缓存。API Key 在请求时读取，更换 Key 不清空 Chat 缓存。
- 消息持久化由 `AgentRunBridge` 交给防抖写入，空闲、出错和卸载时刷新尾部消息。原生已提交消息保留 `data-rcode-messages`，崩溃后不自动重放中断的工具调用。
- 独立对话及草稿使用 `~/.rcode/chat/default`，不自动初始化 Git。草稿首次发送后保留开发工具归属，切换项目时保留仍有工具的原草稿 ID。
- 原生子 Agent 独立会话、可取消、强制只读，工具仅允许 `Read`、`Glob`、`Grep`、`list_directory`，不注册递归委派，也不接入父 Agent 的扩展工具。

## 工具审批与密钥

- 原生 `Write`、`Edit`、`MultiEdit` 在 Rust 审批后直接原子写入，未接入逐块确认。AI SDK 的差异审阅界面不代表所有原生写入前都有差异标签页。
- 原生权限语义：`ask` 逐工具审批状态变更；`edit` 自动批准 `Write`、`Edit`、`MultiEdit` 和 `create_directory`；`full-access` 自动批准状态变更工具。工作区、敏感路径校验和只读 `PlanGuard` 在所有模式下继续生效。
- 前端会话默认归一为 `edit`；原生请求未设置权限时默认 `ask`。不要混淆默认值或削弱非法输入校验。`rcode:` 审批 ID 必须经 `agent_core_approve` 回复。
- 原生 Plan 模式禁止状态变更，工具注册表与执行时的守卫都须保持该约束。前台命令显式传入 cwd，后台进程走有界桥接并及时释放。
- macOS 密钥保存在 Keychain，Windows 保存在 Credential Manager，通过 Rust `keyring` 访问。Linux 当前使用 `~/.rcode/credentials/secrets.json` 的明文 JSON 后端，文件权限为 `0600`；不得将其描述为加密存储。
- 前端统一通过 `secrets_*` 命令访问密钥。不得另存到偏好设置、会话、日志、`localStorage` 或其他业务文件，也不得在示例、测试夹具和提交中写入真实凭据或用户数据。
- RCode 私有文件路径统一由 Rust `crates/rcode-runtime/src/storage.rs` 定义，前端使用 `@/lib/storage`，同步读取使用已水合的 `@/lib/uiState`。不新增 AppData、localStorage、IndexedDB 持久化或旧路径迁移；项目文件、外部 Agent 配置和系统凭据库保持各自所有权。

## 界面与终端修改

- 复用现有组件和设计结构。shadcn/ui 基元位于 `apps/desktop/ui/components/ui/`，AI Elements 位于 `apps/desktop/ui/components/ai-elements/`；升级使用现有生成配置并审查差异，业务组合放对应模块，不手工修改生成的基元。
- Tailwind v4 的 `@theme` 位于 `apps/desktop/ui/styles/globals.css`，`apps/desktop/components.json` 的 CSS 路径必须与该入口一致，不创建 `tailwind.config.*`。组合类名使用 `cn()`。
- 展示文本使用 `useTranslation()`，使语言切换正确更新 React Compiler 缓存。前端跨模块引用使用 `@/`，遵循现有主题令牌和变体。
- `AiComposerProvider` 保持无条件挂载，避免加载密钥时改变父元素类型、重挂整棵树和重复创建 PTY。切换工具标签页保留会话组件，隐藏时释放或暂停展示资源。
- 每个终端叶节点只持有一个持久 Ghostty 模型和 PTY；WebGPU / WebGL 切换不替换模型、选择区、搜索或历史。保留输出背压、最终解析确认、隐藏资源回收和平台进程清理约束。
- LSP 按需启用；没有根标记不启动会话，保持会话数量、空闲回收和崩溃退避的边界。编辑器保存保留 EOL 并检测外部修改，不静默覆盖冲突。
- 跨平台路径在边界归一为正斜杠，解析时接受两种分隔符。平台 Shell 初始化放对应 cfg 分支，终端 Enter 发送 CR。用户目录通过 `dirs` 获取。
- 注释仅解释必要的原因，保持简短。代码、注释、提交和文档不使用破折号或装饰性 emoji；终端本身的 Unicode 展示能力不受此写作规则限制。
- 不把历史包体或内存数据当作当前实测结果。避免空闲轮询和无上限缓存，性能结论附环境、步骤和数据。
- 涉及可见界面或原生集成时，在相同状态和尺寸下验证原生窗口；记录截图与未验证的平台范围。自动化通过不能替代平台和长期运行验收。

## 验证与交付

| 改动范围 | 必要验证 |
| --- | --- |
| 文档或 Agent 规则 | 引用路径、命令、结构和源码一致性检查，`git diff --check` |
| 组件生成配置等元数据 | 解析配置，核实引用文件和实际入口；不为无运行时变化启动完整构建 |
| 前端逻辑或组件 | `pnpm lint`、`pnpm check-types`、`pnpm test`，核心行为增加保护不变量的测试 |
| 前端打包、资源或依赖 | 上述检查加 `pnpm build`；影响包体时执行对应预算检查 |
| Rust 核心或桌面宿主 | 从仓库根目录执行格式、clippy 和受影响包的测试；跨包改动检查 workspace |
| 终端、Shell、授权、Git、文件或 AI 工具边界 | 对应行为测试和真实场景验证；终端另核对专项文档 |

Rust 全 workspace 检查：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

已安装 nextest 时可用 `cargo nextest run --workspace --locked` 运行测试；仍须按改动范围覆盖相关 doctest。打包使用 `pnpm tauri build`，由配置先构建 CLI sidecar 和前端。自动更新及 updater 产物保持禁用，直到具备 RCode 自有且已验证的发布地址和签名密钥；不复用其他应用的更新身份。

复制或改编第三方源码前核对许可证和归属要求，保留必要声明及来源，不通过改名隐藏来源。完成前检查 `git diff`、`git diff --check`，报告实际通过的验证、失败和未验证范围，不把尝试执行视为通过。
