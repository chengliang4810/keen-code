# KeenCode Agent 开发指南

本文指导在本仓库中开发 KeenCode 的 Agent。产品内 Agent 的权限、Plan 模式和子 Agent 机制是实现要求，不代表开发本仓库时自动获得额外授权或必须进入 Plan 模式。

## 开始任务

- 用中文沟通。先确认用户要的是分析、计划还是实现；分析与评审请求不自动授权修改文件。
- 先看 `git status --short`，保留已有改动；搜索优先使用 `rg` / `rg --files`。
- 先读目标模块、调用方、相邻测试和依赖清单，再修改。下表用于定位，不要求每次通读整个仓库。
- 在共同原因处做最小完整修改，不顺手重构无关模块，不增加假设中的扩展点。
- 遇到文档与实现不一致，核实当前代码、清单和 CI，并明确指出差异；不要复制过期说明。

## 当前技术栈与入口

KeenCode 是本地优先的桌面 AI 编码工具：React 19 + TypeScript + Vite 6 前端，Tauri 2 桌面外壳，进程内自研 Rust Agent 运行时。浏览器开发服务器只用于前端开发，不能代替原生桌面验收。

仓库按职责分三层：`apps/`（`ui/` React 界面、`desktop/` 内含 `src-tauri/` 桌面宿主、`cli/` 命令行）、`core/`（共享 Rust 库，Cargo 包名仍为 `keencode-*`）、`tooling/`（`provider-live-test/` 与仓库脚本）。全部 Rust 包属于根 `Cargo.toml` 这一个 workspace，共享一份 `Cargo.lock` 与根 `target/`。

| 改动内容 | 优先检查的位置 |
| --- | --- |
| 应用装配、顶层路由、跨域协调 | `apps/ui/src/App.tsx`、`apps/ui/src/features/app/` |
| 会话发送、停止、队列、导航 | `apps/ui/src/hooks/useSessionTurn.ts`、`apps/ui/src/hooks/session-turn/`、`apps/ui/src/hooks/useSessionNavigation.ts` |
| ACP 客户端、事件、历史与界面投影 | `apps/ui/src/lib/acp/`、`apps/ui/src/hooks/useAcpSessionRuntime.ts`、`apps/ui/src/hooks/acp-runtime/` |
| 业务视图与聊天渲染 | `apps/ui/src/components/`、`apps/ui/src/components/lobe-chat/` |
| 通用控件、样式与翻译 | `apps/ui/src/components/ui/`、`apps/ui/src/styles/`、`apps/ui/src/i18n/` |
| 前端纯规则 | `apps/ui/src/lib/`；测试通常与实现同目录 |
| Tauri 命令注册与桌面集成 | `apps/desktop/src-tauri/src/lib.rs` 及同目录业务模块 |
| 桌面会话接入、历史加载、工具投影 | `apps/desktop/src-tauri/src/agent_runtime.rs`、`apps/desktop/src-tauri/src/agent_runtime/` |
| ACP 方法、事件与协议契约 | `core/acp/` |
| Agent Loop、上下文、取消、Plan 守卫、协作 | `core/agent/` |
| Provider 中立消息、请求、流与错误类型 | `core/model/` |
| 模型协议适配与供应商接入 | `core/provider/`；协议参考在 `docs/protocols/` |
| 会话生命周期、发布与恢复 | `core/runtime/` |
| 会话日志、快照、Artifact、Memory、Goal 持久化 | `core/resources/` |
| 工具定义与执行 | `core/tools/` |
| MCP 与 Skills 核心 | `core/mcp/`、`core/skills/` |
| 插件与扩展桌面接入 | `apps/desktop/src-tauri/src/extensions/`、`apps/desktop/src-tauri/src/plugins/` 及对应 `.rs` 入口 |
| 产品内系统提示词与子 Agent 模板 | `apps/desktop/src-tauri/prompts/`；先读其中的 `README.md` |
| 构建、发布、性能与验收 | `package.json`、`.github/workflows/`、`tooling/scripts/`、`design-qa.md`、`docs/benchmark.md` |

## 架构硬约束

### 前端职责

- `App.tsx` 只做壳层装配、顶层路由和跨业务域协调。单域状态与副作用放对应 hook，纯规则放 `apps/ui/src/lib/`，完整视图放 `apps/ui/src/components/` 或现有业务模块。
- 不以“之后再拆”为由往 `App.tsx` 增加业务规则、协议解析或持久化流程。拆分只覆盖本次涉及的业务，不引入全局状态框架、万能 Context 或只转发 props 的包装层。
- Rust 后端持有权威会话状态；前端只生成可丢弃的界面投影。不要在前端建立第二套会话协议或独立持久化事实源。

### Rust 与协议职责

- 桌面后端通过进程内 ACP Session、请求和事件驱动运行时。标准会话语义使用 `session/*`，KeenCode 专有能力使用 `keencode/*`。
- Agent Loop、工具运行时、Session Store 和 ACP 层保持 Provider 中立，不依赖厂商 SDK 类型，也不按厂商名称分支。
- Anthropic Messages、OpenAI Chat Completions、OpenAI Responses 的请求、流、工具调用、结构化输出、Usage 和错误归一由各自 Adapter 负责。
- 修改事件或数据契约时，同时检查 Rust 定义、桌面转发、前端解码与投影、持久化恢复和相关测试；不要只改一端。
- 文件系统、进程、PTY、Git、密钥和操作系统能力留在 Rust 系统能力边界，界面不直接实现模型协议或工具调度。

### 产品语义

- 会话相互隔离：Goal、Todo、Plan 状态属于单个对话，不能提升为项目共享状态。子 Agent 只支持单层，不加入递归或 DAG 工作流。
- 产品内工具执行不维护信任状态、权限模式或审批分支。加入项目后可在宿主进程权限内访问项目内外路径；文件修改、命令和网络行为必须可审查。
- Plan 开启时只读调研；用户关闭并发起实施后执行，不增加独立计划审批。只读方案与报告放 `~/.keencode` 下按项目划分的沙箱，不写入用户项目。
- 本地记忆正文和索引放 `~/.keencode/memories`，不增加独立 Memory Sideagent、Ambient Memory 或本地 Embedding。
- 插件市场、Skills、MCP 是同级入口。模型设置只管理自定义供应商，不实现厂商官方账户、套餐、额度或官方 Key。
- 用户明确说明已公开发布前，按全新项目维护唯一配置和数据结构，不加入旧字段迁移、历史路径回退、枚举别名或废弃 API 包装。
- 不扩展为 Web/SaaS、团队管理、外部消息渠道或多层 Agent 编排；不默认打包浏览器内核、外部脚本运行时或模型权重。

## 界面修改

- 直接复用当前 `apps/ui/src/`、`apps/ui/public/` 的组件、DOM、CSS、设计令牌和资源，不根据截图重写近似实现。
- 可见交互控件使用 `apps/ui/src/components/ui/` 中的 shadcn/ui 组件。业务代码不新增原生 `button`、`input`、`textarea`、`select`、`dialog`，也不重复实现已有控件；缺失时先查询当前版本文档，优先组合已有组件或使用正常包依赖，遵守下文的源码来源边界。
- 原生控件仅限 UI 组件底层、浏览器要求的隐藏控件，以及 `contenteditable`、媒体等无等价组件的宿主；在代码中解释原因，不另建可见控件样式。
- 使用组件既有变体、尺寸和语义化令牌。业务 `className` 只负责必要布局与产品结构，不覆盖控件颜色、字体、边框、圆角和交互状态。
- 后端或协议调整不得无意改变界面。品牌或文案变化保留原盒模型与层级，并记录有意差异。
- 可见界面修改按 `design-qa.md` 记录基线提交或源码快照、运行环境、截图位置、复现命令；在相同状态、视口和 `deviceScaleFactor` 下比较截图与像素差异。
- 缺失基线先尝试隔离重建；无法重建则记录原因和未验证范围，继续其他验证。历史记录和浏览器截图不能代替本次原生桌面验收。

## 开发与验证命令

命令均从仓库根目录执行。pnpm 固定为 `10.14.0`；当前 CI 使用 Node.js 24、Rust stable，Tauri 还需要所在平台的系统构建依赖。环境要求以清单和 `.github/workflows/ci.yml` 为准。

```sh
corepack pnpm@10.14.0 install --frozen-lockfile
pnpm dev:desktop   # 原生桌面开发
pnpm dev           # 仅前端开发
```

以下示例使用 `pnpm`；Windows 使用 `pnpm.cmd`。未安装独立 pnpm 时可替换为 `corepack pnpm@10.14.0`。

| 改动范围 | 必要验证 |
| --- | --- |
| 仅文档或 Agent 规则 | 引用路径、命令与结构检查，`git diff --check`；不启动无关构建 |
| 前端逻辑或组件 | `pnpm run typecheck`，`pnpm exec vitest run --root apps/ui <apps/ui 内测试文件路径>` |
| 样式 | 上述相关检查，加 `pnpm run lint:css` 与界面基线比较 |
| 完整前端验证 | `pnpm test`、`pnpm build`；`pnpm test` 包含脚本测试、clean-room 检查与 Vitest |
| 核心 Rust crate | `cargo test -p <包名>` |
| Tauri 后端 | `cargo test -p keencode-desktop` |
| 跨 crate 或公共协议 | 检查所有受影响的包；涉及桌面接入时另跑 `cargo test -p keencode-desktop` |

**唯一的 Rust workspace**：根 `Cargo.toml` 统一管理 `core/`、`apps/desktop/src-tauri` 和 `tooling/provider-live-test`，共享一份 `Cargo.lock` 与根 `target/`。前端检查不能代替 Rust 测试；首次构建桌面端或执行 `cargo check` 前需要 `apps/ui/dist` 存在（`pnpm build` 或 `mkdir -p apps/ui/dist`），因为 Tauri 构建脚本在编译期校验打包资源。

Rust 修改还需做格式检查和 lint：

```sh
cargo fmt --all -- --check
cargo clippy -p <包名> --all-targets -- -D warnings
```

完整 CI 的 workspace、all-targets 和 doctest 矩阵见 `.github/workflows/ci.yml`。真实模型测试入口在 `tooling/provider-live-test/`，不要把需要凭据和网络的测试当作离线单测。

## 依赖与参考项目边界

- 允许通过 npm/pnpm、Cargo 正常声明和使用第三方依赖，遵守其许可证；优先复用已有依赖。
- 经用户明确授权，KeenCode 前端可以直接复用 Apache-2.0 许可的 ZCode UI 组件与 CSS，以 ZCode 作为桌面和 Web 界面的视觉基线；复用范围不扩展到协议、运行时、提示词、凭据或产品数据。
- 复制或改编第三方源码时，必须核对许可证，保留必要的版权与许可证文本，并在 `THIRD_PARTY_NOTICES.md`、`DESIGN.md` 或对应源码附近记录来源和影响范围。不得通过改名、删除声明或放宽门禁隐藏来源。
- 其他未获明确授权的外部项目仍仅用于参考；需要引入其源码时必须先确认许可与归属要求。
- `pnpm run check:clean-room` 仅检查部分来源名称与参考描述，不能证明源码原创，也不能把所有关键词命中认定为复制。来源判断需要结合文件内容、引入历史和实际引用。

## 实现与交付标准

- 优先标准库和已有依赖。新增依赖必须说明必要性及对体积、启动、内存和维护的影响；不凭经验宣称性能提升。
- 无任务时不增加持续轮询；后台进程、监听器、连接按需启动并及时释放。首版目标：安装包不超过 `50 MB`、空闲 CPU 低于 `1%`、首个可交互窗口冷启动不超过 `1.5 秒`。这些是预算，不是已达标结论。
- 性能结论附环境、步骤和数据；记录空闲、单活跃会话与并发会话增量内存。突破预算前提供实测收益和替代方案。
- 在外部输入、路径、命令和协议边界做校验；凭据与用户数据不得进入日志、测试夹具或提交。数据默认本地保存；新增遥测或数据共享必须明确说明并可关闭。
- 关键行为变更增加能保护行为的测试，避免只复述实现。失败时修复原因，不跳过检查、削弱断言或屏蔽诊断来制造通过。
- 完成前审查 `git diff` 和 `git diff --check`。报告改了什么、验证结果及未验证范围，不把尝试执行当作通过。
- 仅在用户要求时提交或推送。获准提交后按功能点拆分，提交信息使用中英双语，只暂存本次相关改动；不擅自 amend、重写历史或绕过 hooks。


## Appica UI（强制规则）

Appica UI component index (fetch before using a component you haven't used before):
https://appica.dev/ui/react/llms.txt

- Tailwind CSS v4 only. Do NOT create a `tailwind.config.js` - v4 config lives in CSS via `@theme`.
  If the project is on v3, convert unsupported syntax rather than downgrading the components.
- Scan the library for class names or everything renders unstyled: `@source '../node_modules/@appica/ui-react/dist';`
  in the stylesheet that imports Tailwind. The path is relative to that stylesheet - count the `../`
  needed to reach the project root. A bare package name resolves to nothing and fails silently.
- React 19 is a hard requirement. No `forwardRef` - `ref` is a plain prop.
- Import from the subpath, one component per import:
  `import { Button } from '@appica/ui-react/button'`.
- **尺寸统一**：所有直接使用或由本地 wrapper 封装的 `@appica/ui-react/*` 原语，其 `size` 与 `inputSize` 必须统一并显式写为 `md`，分别使用 `size="md"` 或 `inputSize="md"`；不得省略尺寸、使用其他尺寸变体、传入像素表达式，或通过样式覆盖模拟其他尺寸。KeenCode 的本地 UI wrapper 内部同样必须传 `md`，wrapper 自身的语义尺寸不应透传为 Appica 的非 `md` 尺寸。
- 该尺寸规则适用于业务层直接使用和本地封装的 Appica 组件；提交前运行 `pnpm run check:design-system`，由 `DSG005` 门禁阻止未批准的尺寸变体。图标的像素 `size` 不属于 Appica 组件尺寸规则。
- Never write hex colors, px radii, or duration literals. Use the role-based tokens:
  `bg-background-muted`, `text-foreground-intense`, `border-border-strong`, `var(--radius-md)`.
  Full list: https://appica.dev/ui/docs/react/colors.md
- Never write hue-based utilities (`bg-gray-100`, `text-slate-600`). The palette is organized by
  role, not hue.
- Prefer v4 variant syntax (`*:`, `**:`, `data-*:`, `not-*:`) over `[&_...]` arbitrary selectors.
- For a link styled as a button, put `buttonVariants(...)` on the `<a>` - never `<Button render={<a/>}>`.
- Put `className` overrides on the wrapper component, not on the JSX passed to `render`.
- Do not hand-roll a component that exists in the library. Check the component list first:
  https://appica.dev/llms.txt
- Every documentation page is served as clean markdown at `<url>.md` - fetch that, not the HTML.
