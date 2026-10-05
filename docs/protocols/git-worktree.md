# 本地 Git Worktree 协议

KeenCode 的工作树入口只面向本地、已登记的 Git 项目。renderer 通过
`IPlatformService` 调用 Tauri 命令，Rust 负责 canonical path、仓库身份、Session 归属、
Journal 回执和物理 checkout 校验；前端不保存工作树事实，也不执行 Git/Node 进程。

## 查询与创建

`listGitWorktrees(projectPath)` 映射 `git_worktrees_list({ projectPath })`，返回
`{ available, worktrees, reason? }`。每个条目包含 `path`、`head`、`branch`、`detached`、
`isMain`、`locked` 和 `prunable`。

`createGitWorktree(input)` 映射 `ui_git_worktree_create({ input })`。输入严格使用
`cwd`、`ref`、`path?`、`newBranch?`、`copyChangesFrom?`、`progressId?` 和
`checkoutBranch`。成功返回 `worktree.path/ref/branch`，Rust 会写入 managed checkout
身份标记，后续归档清理只接受该标记。

KeenCode 新增的 `packages/ui/src/ComposerWorktreeMenu.tsx` 位于 workspace Composer
的 Git 上下文旁，并复用固定 Source 的 Dropdown/Input/Dialog 控件和 DOM 语义；活动会话
时同一菜单也出现在 Composer 输入区，避免 handoff 只能在草稿态显示。`workspacePath`
切换必须经过 Root 的 workspace 选择链，不能由 renderer 直接改当前会话目录。

## Session 交接

`handoffGitWorkspace(input)` 映射 `ui_git_handoff({ input })`，输入字段采用 Rust 的
camelCase `commandId/threadId/cwd/targetMode` 及工作树 branch/ref 字段。交接前 UI 先
调用必需的 `stopGitWorkspaceSession(sessionId)` 关闭执行资源，Rust 随后把同一 Session
的权威 project root 改到目标 checkout 并返回 `associatedWorktreePath` 等回执。已有工作树
条目只有在携带匹配的 Session association proof 时才可直接交接；当前菜单无法证明该关联
时只提供 Rust 创建并绑定新工作树的 handoff。UI 重新注册目标 workspace 后按原 Session
ID 选择会话。

## 归档与移除

归档先调用 `stopGitWorkspaceSession(sessionId)`，再使用 `archiveTaskWithReceipt` 取得同一次
`SessionPreferenceSet` 的 `operationId` 和 `journalSequence`，最后调用
`archiveGitWorktree({ cwd, path, threadId, operationId, journalSequence })`。Rust 会再次
校验 Journal、Session 状态、managed 身份和 checkout 内容，任何回执不匹配都保留目录。

普通移除调用 `removeGitWorktree({ cwd, path, force: false, reclaimTemporaryBranch:
false })`。缺失目录的 prunable 条目不从菜单发起普通移除；Rust 仍拒绝主 checkout、非
关联目录、仍被 Session/终端引用或包含未提交/忽略文件的目标。恢复记录仅通过
`listGitWorktreeArchiveRecords` 和
`recoverGitWorktree(sessionId)` 读取，不由 renderer 重建。`reclaimTemporaryBranch` 作为
现有 Source 字段保留，但不再支持旧的临时分支自动回收；传入 `true` 会在任何 Git 或
文件系统操作前明确拒绝并保留工作树与分支。
