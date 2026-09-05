# 2026-09-05 阶段 1.4 重复代码检测与复用优化报告

## 结论

阶段 1.4 以 `7406981`（阶段 1.3 完成）为起点，对桌面前端、Tauri 后端和供应商化 Peri 运行时进行跨文件重复扫描，并把已确认的重复实现迁移到共享入口。改造坚持“先锁定行为与错误契约，再替换全部调用点”：公共抽象不是演示层包装，而是直接承接原调用方的权限、超时、回滚、大小写、进程树、事件顺序和跨平台边界。

本阶段分两批落地：

- `adcffaf` 保存第一批经过验证的复用基线，并移除全新项目不应保留的 Smart Compact/v1 空兼容路径；
- 本报告对应第二批收口，补齐原子文件、外部进程、插件标识与市场缓存、模型请求观测、Session 元数据、LSP 测试夹具、只读子 Agent 契约和工具时间线解析等统一入口。

## 扫描范围与方法

| 范围 | 检查重点 |
| --- | --- |
| `src/` | 重复的搜索控件、工具输入 JSON 解析、路径展示、React ref/文本提取、状态持久化、Host 快照归并和 CSS 选择器 |
| `src-tauri/src/` | 原子写入、路径安全校验、Git/系统子进程、有限 HTTP 读取、插件来源解析、Provider/项目/记忆持久化 |
| `vendor/peri/` | `PluginId`、Marketplace/插件配置、外部进程生命周期、LSP 子进程、请求重试与观测、ThreadStore 元数据、子 Agent 能力契约 |
| 测试与清单 | 重复脚本夹具、重复错误断言、Cargo 依赖闭包、所有调用点和平台条件分支 |

扫描采用以下证据交叉确认，不把相同词汇或必要的协议适配器误报为重复代码：

1. 用 `rg --files`、`git ls-files` 和行数统计建立源码清单，排除构建产物、二进制文件和供应商统一补丁文本。
2. 搜索重复的命令构造、临时文件加 `rename`、路径段校验、`plugin@marketplace` 拼接、JSON 读改写、请求 attempt 事件、数据库行到 Session 元数据映射、工具输入解析和只读提示段。
3. 对完全相同片段按调用点回溯；对高度相似逻辑按输入、输出、失败阶段、并发和平台语义比较，只有契约可统一时才抽取。
4. 抽取后反向搜索旧 helper、裸 `Command::new`、直接 `PluginId` 构造、固定临时文件名和重复提示文本，确认生产调用方全部迁移或记录必要例外。
5. 以定向单测、工作区全量测试、严格 Clippy、前端类型检查/构建和固定视口像素证据验证行为没有被“去重”简化。

## 已统一的重复实现

### 第一批复用基线

| 重复族 | 统一入口 | 已替换范围 |
| --- | --- | --- |
| 列表搜索输入及清空交互 | `src/components/SearchField.tsx` | 分支菜单、轨迹账本、资源树等搜索入口 |
| 应用设置乐观写入与回滚 | `src/lib/appSettingPersistence.ts` | `useAppSettings` 中按字段串行、revision 判定、后端确认值和失败回滚 |
| Host 活跃回合快照归并 | `src/lib/activeTurn.ts` | `App` 与 Session turn hook 的恢复/增量状态对齐 |
| 有界 HTTP 响应读取 | `src-tauri/src/http_response.rs` | 下载、模型目录和 MCPB 读取；统一 `Content-Length` 预检及 chunked 上限 |
| Web fetch/search 响应处理 | `vendor/peri/peri-middlewares/src/middleware/web_common.rs` | 两种 Web 工具共享状态、正文、错误和截断规则 |
| React ref、文本和文件路径小工具 | `src/lib/reactRefs.ts`、`reactNodeText.tsx`、`filePath.ts` 等 | 删除组件内重复实现并保留 UI 盒模型 |
| Compact 与 v1 空兼容层 | 唯一 Micro/Full Compact 契约 | 删除 Smart 分支、旧字段和无操作桥，不保留新项目无效兼容代码 |

### 第二批收口

| 重复族 | 统一入口 | 关键行为 |
| --- | --- | --- |
| Tauri 私有/普通文件原子写入 | `src-tauri/src/storage.rs` | 同目录唯一临时文件、同步、可靠覆盖、私有权限、失败保留旧目标 |
| Peri 普通/私有文件原子写入 | `peri-middlewares::atomic_file` | 普通文件 `0666 & umask`、私有配置 `0600`、已有权限保留或收紧、`tempfile::persist` |
| 跨平台安全路径判断 | 两侧 `path_utils` 与 `PluginId` 合法组件契约 | 拒绝绝对路径、父级逃逸、控制字符、Windows 设备名、尾点和大小写碰撞 |
| 插件身份与存储键 | `peri_acp_types::plugin::PluginId` | 展示大小写与大小写无关 identity 分离，统一解析/比较/序列化和稳定哈希存储组件 |
| Marketplace Git/npm 取得 | 共享缓存目录、RAII 临时目录和进程 runner | 禁止脚本与交互输入；失败、超时、取消不提升半成品、不泄漏临时目录 |
| Tauri 短命令生命周期 | `src-tauri/src/process_lifecycle.rs` | stdin 关闭、输出有界排空、分类超时、Unix 进程组、Windows Job Object 和根进程回收 |
| Peri Tokio 短命令生命周期 | `peri-middlewares::process_lifecycle` | Hook、Marketplace、Git 共用总体 deadline、并发输出排空、future drop 与整树清理 |
| MCP/LSP 长驻子进程 | `process-wrap` 平台 wrapper 与共享 Rust LSP fixture | Unix 进程组、Windows 挂起创建后绑定 Job Object、同步 Drop、取消与 broken-pipe 回归 |
| 模型请求 attempt 观测 | `RequestLifecycle` 与统一 retry stream | Anthropic、OpenAI Compatible、Responses 共享 logical/attempt 顺序、取消和重试收口 |
| Session 元数据恢复 | `peri-resources/src/sessions/mod.rs` | Filesystem/SQLite 共用行数据到 `ThreadMeta` 的校验与默认值 |
| 只读子 Agent 契约 | `subagent/built-in/read-only-contract.md` | explorer、plan、verification 共享只读与报告路径约束，并以测试防漂移 |
| 工具时间线输入解析 | `src/lib/toolDisplay.ts` | 摘要与 `TimelineToolRow` 共用容错 JSON 字段、`toolName` 和跨平台 basename |

## 调用点完整性与保留例外

- 项目文件写入保留普通文件权限；Provider、MCP、插件设置、记忆及应用配置走私有入口，不能用一个默认权限掩盖两种安全边界。
- Marketplace 展示名保留原始大小写；比较、缓存和安装 identity 使用同一 `PluginId` 规则，避免 UI 名称被存储规范化意外改写。
- 生产 Git/npm/Hook 调用必须进入具备无交互、总体超时和整树清理的 runner。构建脚本编译 Rust 测试 fixture、测试自身启动 shell，以及集中 helper 内部创建命令属于明确例外。
- Filesystem 与 SQLite 仍保留各自的 I/O/SQL 适配层；共享的是 Session 元数据契约，不建立只转发单一实现的接口。
- 两处 JSON 序列化薄适配器可保留各自错误类型；底层持久化已经统一，继续抽象不会减少生命周期或权限逻辑。
- Tauri 与 Peri 位于不同 crate/依赖边界，各自保留平台 runner；两侧都复用 `peri-agent` 的进程树能力，不为了形式上的单文件去重制造反向依赖。

## 验证结果

| 门禁 | 结果 |
| --- | --- |
| Peri 格式、构建与严格 lint | `cargo fmt --all -- --check`、workspace `cargo check --all-targets`、workspace Clippy `-D warnings` 均通过 |
| Peri 全量测试 | workspace `--all-targets` 共 2647 通过、0 失败、4 忽略；其中 `peri-middlewares` 1245 通过、3 忽略 |
| Tauri 格式、构建与严格 lint | `cargo fmt --all -- --check`、workspace `cargo check --all-targets`、workspace Clippy `-D warnings` 均通过 |
| Tauri 全量测试 | 267 通过、0 失败、0 忽略；阶段 0 记录的 5 项 Windows 失败均已恢复绿色 |
| 前端测试 | `npm.cmd test` 中 Node 8 项通过，Vitest 118 个文件、970 项通过 |
| 前端静态与构建 | TypeScript、Stylelint、生产构建均通过；构建仍报告 2 组动态/静态导入冲突和 6 个超过 500 kB 的产物，最大主包 1,607.46 kB |
| 完整性扫描 | `git diff --check` 通过；扫描 969 个源码文件，无超过 4000 行文件；`eslint-disable` 为 0；生产代码无直接 `PluginId { ... }` 构造 |
| 外部命令反向扫描 | 生产 Git/npm/Hook 全部进入共享 runner；裸命令只存在于统一构造器、测试/构建夹具和有意 fire-and-forget/长驻进程路径 |
| 固定视口像素证据 | 1440×900 基线、当前和最终截图文件字节一致，SHA-256 均为 `A20F323337277D480658691243B90C1BA4CE09B7D76D0D9FD729F76D3B6DE578`，像素差 0 |
| 供应商统一补丁 | `0001-keencode-current.patch` 已从固定上游 `ef45872c0a725ef8acda5afffb6e45cabeeff9e3` 的真实 Git 树 `73cfbb4b5f77ddd134419cd5eacf3ecd787bb5e9` 重新生成；在该树上通过 `git apply --check --index --whitespace=error-all` 和实际索引应用，应用后树 `ed3574a9e1175af762cee893d95960e5eb68033b` 与 `18c59c9:vendor/peri` 完全一致；补丁 blob 为 `a3ba05cef721c5989e0f049966c2b285672a0901` |

验证边界：当前 Windows 未安装 macOS Rust target，macOS PATH 非阻塞 fd 分支只完成源码审查，尚未做目标平台编译；Tauri 中两个 Unix 进程组回归受 `cfg(unix)` 限制，Windows 本轮未执行。固定截图只覆盖欢迎页外壳，不代表 Webview、滚动、PTY、Marketplace 或 MCP/LSP 的原生桌面全功能 E2E。

## 残余风险与后续阶段边界

- 欢迎页固定视口截图可以证明本次复用没有改变应用壳层，但不能替代真实工具时间线、Marketplace、MCP/LSP 和 Git 进程生命周期的原生桌面路径验证；这些边界继续纳入阶段 2 和阶段 5。
- 动态/静态导入冲突和大 chunk 提示属于既存打包问题，需要在阶段 5.3 结合体积数据处理，不能在复用阶段随意改变加载时序。
- 仅测试夹具、构建脚本或共享命令构造器内部允许保留裸命令创建；新增生产路径必须复用统一生命周期入口。
- 供应商统一补丁已经覆盖 `18c59c9:vendor/peri` 的最终代码树，并包含固定上游中的 2 个 Gitlink 删除和 7 个可执行文件删除，避免归档解压在 Windows 上丢失模式或子模块差异。阶段 1.4 的代码、文档与供应商修改登记至此完成。
