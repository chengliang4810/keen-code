# Native GPUI Git Worktree 契约

本文描述当前原生工作台的 Git linked worktree 服务。生产入口是
`NativeWorkbenchPanel` 持有的 `NativeWorktrees`，由 `NativeHost` 在 GPUI 启动时注入；
没有 Tauri command、`IPlatformService`、VQL 消息或前端 worktree store。

实现锚点：`apps/desktop/src/native_ui/workbench/worktrees.rs`、
`apps/desktop/src/native_ui/workbench/panel.rs`、
`apps/desktop/src/native_ui/workbench.rs` 和
`apps/desktop/src/native_host/launch.rs`。

## Service API

`NativeWorktrees` 直接使用 `Result<T, String>` 返回结果。`NativeWorkbenchPanel` 将
调用放入 blocking 任务，检查当前 workspace generation，成功后重新读取工作台快照；
失败只更新错误投影，不修改本地伪状态。

| 方法 | 输入 | 成功结果 |
| --- | --- | --- |
| `list(root)` | 当前项目根 | `Vec<WorktreeEntry>` |
| `create(WorktreeRequest)` | root、reference、可选目标路径/新分支/复制来源、`checkout_branch` | `WorktreeResult` |
| `remove(root, target, force)` | 当前仓库根、linked worktree、强制标记 | `()` |
| `handoff(source, target, target_branch?)` | 同一仓库的源/目标工作树和可选分支 | 目标 `PathBuf` |
| `archive(root, target, session_id)` | 受管工作树和非空会话标识 | 归档 receipt `PathBuf` |

`WorktreeEntry` 包含 `path`、`head`、`branch`、`detached`、`is_main`、`locked` 和
`prunable`。创建结果包含 `path`、原始 `reference`、可选 `branch` 和
`copied_changes`。这些 Rust 结构是当前 typed service 契约；面板显示的中文状态文本
不是稳定协议值。

## 边界与失败语义

- 已存在的 root、source 和待操作目标先 canonicalize；创建目标使用 canonicalize 后的
  父目录，并拒绝符号链接或 reparse point，创建完成后再 canonicalize 实际工作树。
  引用、分支名和目标路径有独立校验。
- 创建前解析 commit，目标已存在时直接拒绝；新分支只在本次创建成功时允许回滚。
  创建完成后写入受控 managed 标记，再按请求复制本地修改；复制失败会保留已创建
  工作树并报告路径。
- 普通移除会拒绝未提交或 ignored 文件；`force` 只由面板的明确操作传入。主工作树、
  非 linked worktree、路径越界或仍不满足 Git 校验的目标均拒绝。
- `handoff` 要求源和目标属于同一 Git common root。源有修改时使用受控 stash；切换
  或恢复失败会保留 stash 并返回可诊断错误，不把目录状态伪装为完成。
- `archive` 先写入 `native-worktree-archive/<hash(session_id)>.json` receipt，再尝试
  移除 checkout。移除失败时 receipt 保留，调用方不能据成功 toast 推断目录已删除。

工作台写操作在同一面板内串行；切换项目或关闭面板后，旧任务结果不会覆盖新根目录。
面板没有跨窗口共享的工作树缓存，Host 仍持有服务和后台资源的生命周期。

## 历史：Tauri/Source 名称（已退役）

旧文档中的 `IPlatformService`、`listGitWorktrees`、`createGitWorktree`、
`handoffGitWorkspace`、`archiveGitWorktree`、`removeGitWorktree`、
`ui_git_worktree_create` 和 `archiveTaskWithReceipt` 不是当前实现的调用入口。它们只
保留在历史 diff 或旧验收材料中；新代码应直接引用上面的 `NativeWorktrees` 方法，
需要会话动作回执时使用 `NativeHostApi::dispatch` 的 `NativeActionReceipt`，不要重新
建立旧桥接层。

旧 Source 菜单曾记录 `packages/ui/src/ComposerWorktreeMenu.tsx`、
`ui_git_handoff`、`stopGitWorkspaceSession`、Session association proof、
`archiveTaskWithReceipt` 和 `recoverGitWorktree` 等流程；这些名称与流程只保留用于
迁移审查和旧验收报告的定位。当前 GPUI 面板没有该菜单或桥接回执，不能据此宣称已有
Session 交接、Journal 对账或恢复 UI 验收。
