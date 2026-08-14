/** 当前 Tauri 后端的类型化调用入口。 */

export function isTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    ("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  );
}

/** 调用当前桌面后端注册的 Tauri 命令。 */
async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) throw new Error(`Tauri required: ${cmd}`);
  const { invoke: inv } = await import("@tauri-apps/api/core");
  return inv<T>(cmd, args);
}

/** 请求退出；有运行中任务时由应用展示确认提示。 */
export async function appRequestExit() {
  return invoke<number>("app_request_exit");
}

/** 停止所有运行中任务及其终端进程，然后退出。 */
export async function appConfirmExit() {
  return invoke<void>("app_confirm_exit");
}

/** Peri TaskManager 当前登记且仍在运行的后台任务类别。 */
export type BackgroundTaskKind = "shell" | "agent" | "workflow";

/** 一个由当前 Peri 运行时拥有且仍在运行的后台任务。 */
export interface BackgroundTaskInfo {
  /** 拥有该后台任务的根 Session。 */
  sessionId: string;
  /** Peri TaskManager 分配的稳定任务标识。 */
  taskId: string;
  /** 决定图标、文案与取消语义的任务类别。 */
  kind: BackgroundTaskKind;
  /** 任务启动时记录的单行摘要。 */
  summary: string;
  /** 任务启动时间（UTC RFC 3339）。 */
  startedAt: string;
  /** 查询时已经运行的毫秒数。 */
  durationMs: number;
  /** 仅后台 Shell 具有的系统进程标识。 */
  pid: number | null;
}

/** 查询所有已加载 Session 中仍在运行的后台任务。 */
export async function backgroundTasksList() {
  return invoke<BackgroundTaskInfo[]>("background_tasks_list");
}

/** 通过 Peri Host RPC 精确取消一个后台任务。 */
export async function backgroundTaskCancel(sessionId: string, taskId: string) {
  return invoke<void>("background_task_cancel", { sessionId, taskId });
}

/** 取消查询时仍在运行的全部后台任务。 */
export async function backgroundTasksCancelAll() {
  return invoke<void>("background_tasks_cancel_all");
}

/** 内置终端使用系统 PTY；字节数组避免流式 UTF-8 在分块边界损坏。 */
export async function terminalCreate(
  id: string,
  cwd: string,
  cols: number,
  rows: number,
) {
  return invoke<void>("terminal_create", { id, cwd, cols, rows });
}

export async function terminalWrite(id: string, data: number[]) {
  return invoke<void>("terminal_write", { id, data });
}

export async function terminalResize(id: string, cols: number, rows: number) {
  return invoke<void>("terminal_resize", { id, cols, rows });
}

export async function terminalClose(id: string) {
  return invoke<void>("terminal_close", { id });
}

/** 当前构建版本以及最近一次 GitHub Releases 检查结果。 */
export const APP_UPDATE_STATUS_EVENT = "app://update-status";

export interface AppUpdateStatus {
  /** Tauri 用于跨平台比较的三段数字版本。 */
  currentVersion: string;
  /** 用户可见的日期与提交短哈希版本。 */
  currentRelease: string;
  /** 当前结果是否来自一次完整的联网检查。 */
  checked: boolean;
  /** 是否存在比当前构建更新的签名版本。 */
  available: boolean;
  latestVersion: string | null;
  latestRelease: string | null;
  notes: string | null;
  publishedAt: string | null;
  /** 后台安装包所处阶段；ready 表示已经下载并通过签名校验。 */
  downloadState:
    | "idle"
    | "downloading"
    | "verifying"
    | "ready"
    | "installing"
    | "failed";
  /** 已接收的安装包字节数。 */
  downloadedBytes: number;
  /** 服务端提供的安装包总字节数；未知时为空。 */
  totalBytes: number | null;
  /** 当前实际使用的安装包下载源。 */
  downloadSource: "github" | "chinaMirror" | null;
  /** 后台下载、签名或缓存失败信息。 */
  downloadError: string | null;
}

export type AppUpdateDownloadSource = "auto" | "github" | "chinaMirror";

/** 只读取当前构建版本，不访问网络。 */
export async function appUpdateInfo() {
  return invoke<AppUpdateStatus>("app_update_info");
}

/** 检查 GitHub Releases；发现更新后由后端立即开始后台预下载。 */
export async function appUpdateCheck() {
  return invoke<AppUpdateStatus>("app_update_check");
}

/** 安装已经在后台下载并通过签名校验的更新。 */
export async function appUpdateInstall() {
  return invoke<void>("app_update_install");
}

/** KeenCode 当前登记的项目。 */
export interface ProjectRecord {
  /** 项目稳定标识。 */
  id: string;
  /** 项目显示名称。 */
  name: string;
  /** 项目规范化绝对路径。 */
  path: string;
  /** 项目目录当前是否可访问。 */
  pathOk: boolean;
  /** 项目是否固定在列表顶部。 */
  pinned: boolean;
}

/** 返回当前登记的项目。 */
export async function projectsList() {
  return invoke<ProjectRecord[]>("projects_list");
}

/** 登记一个本地项目目录。 */
export async function projectAdd(path: string) {
  return invoke<ProjectRecord>("project_add", { path });
}

/** One linked git worktree from `git worktree list --porcelain`. */
export interface GitWorktreeEntry {
  path: string;
  head?: string;
  branch?: string;
  detached: boolean;
  isMain: boolean;
  locked: boolean;
  prunable: boolean;
}

export interface GitWorktreesResult {
  available: boolean;
  worktrees: GitWorktreeEntry[];
  reason?: string;
}

/** List worktrees for a project folder. Soft-fails when git/repo missing. */
export async function gitWorktreesList(projectPath: string) {
  return invoke<GitWorktreesResult>("git_worktrees_list", { projectPath });
}

/** Result of creating a linked worktree (`git worktree add`). */
export interface GitWorktreeAddResult {
  path: string;
  name: string;
  startPoint?: string;
  branch?: string;
}

/**
 * Create a linked worktree for a project folder.
 * Path: `<parent>/<main_basename>-<name>` (see docs/llm-wiki/git-worktrees.md).
 * Throws when not a git repo / git missing / path exists / invalid name.
 */
export async function gitWorktreeAdd(
  projectPath: string,
  name: string,
  startPoint?: string | null,
) {
  return invoke<GitWorktreeAddResult>("git_worktree_add", {
    projectPath,
    name,
    startPoint: startPoint?.trim() || null,
  });
}

export async function pickDirectory() {
  return invoke<string | null>("pick_directory");
}

/** Native multi-file picker for composer attachments (empty if cancelled). */
export async function pickAttachFiles() {
  return invoke<string[]>("pick_attach_files");
}

export interface PathEntry {
  path: string;
  name: string;
  isDir: boolean;
  exists: boolean;
}

/** Classify absolute paths as file/dir for drag-drop. */
export async function pathsClassify(paths: string[]) {
  return invoke<PathEntry[]>("paths_classify", { paths });
}

/** Open with OS default app. */
export async function pathOpen(path: string) {
  return invoke<void>("path_open", { path });
}

/** 使用系统默认浏览器打开 HTTP 或 HTTPS 地址。 */
export async function urlOpen(url: string) {
  return invoke<void>("url_open", { url });
}

/** Reveal in Finder / Explorer. */
export async function pathReveal(path: string) {
  return invoke<void>("path_reveal", { path });
}

/** Optional git unified diff for a project file (session Changes panel). */
export interface GitFileDiffResult {
  available: boolean;
  diff?: string;
  relativePath?: string;
  reason?: string;
}

export async function gitFileDiff(projectPath: string, path: string) {
  return invoke<GitFileDiffResult>("git_file_diff", { projectPath, path });
}

/** One workspace file from `git status --porcelain` (Changes → Workspace). */
export interface GitStatusEntry {
  path: string;
  absolutePath: string;
  status: string;
  indexStatus: string;
  worktreeStatus: string;
  kind: string;
  name: string;
  originalPath?: string;
}

export interface GitStatusResult {
  available: boolean;
  files: GitStatusEntry[];
  branch?: string;
  reason?: string;
  /** 所有变更文件的累计新增行数。 */
  additions: number;
  /** 所有变更文件的累计删除行数。 */
  deletions: number;
  /** 是否存在未暂存的变更。 */
  hasUnstagedChanges: boolean;
}

/** Soft-fail workspace git status for the project path. */
export async function gitStatus(projectPath: string) {
  return invoke<GitStatusResult>("git_status", { projectPath });
}

/** File content at HEAD (before snapshot for local unified diffs). */
export interface GitShowFileResult {
  available: boolean;
  content?: string;
  relativePath?: string;
  reason?: string;
}

export async function gitShowFile(projectPath: string, path: string) {
  return invoke<GitShowFileResult>("git_show_file", { projectPath, path });
}

/** Git 提交结果。 */
export interface GitCommitResult {
  /** 提交短哈希。 */
  commit: string;
  /** 提交时的当前分支。 */
  branch: string;
  /** Git 命令输出。 */
  output: string;
}

/** 提交当前项目；includeUnstaged 为 true 时一并带上未暂存变更。 */
export async function gitCommit(opts: {
  /** 项目绝对路径。 */
  projectPath: string;
  /** 提交信息。 */
  message: string;
  /** 是否包含未暂存变更。 */
  includeUnstaged: boolean;
}): Promise<GitCommitResult> {
  return invoke<GitCommitResult>("git_commit", opts);
}

/** 推送当前分支到远端。 */
export async function gitPush(projectPath: string) {
  return invoke("git_push", { projectPath });
}

export interface FsEntry {
  name: string;
  relativePath: string;
  isDir: boolean;
  size: number;
  ext: string;
}

export interface FsReadResult {
  relativePath: string;
  name: string;
  /** Absolute path for convertFileSrc streaming (video/audio/large images). */
  absolutePath: string;
  size: number;
  kind: string;
  mime: string;
  text: string | null;
  base64: string | null;
  /** Prefer asset-protocol stream instead of base64 embed. */
  stream: boolean;
  truncated: boolean;
  error: string | null;
  /** Last modified (ms since epoch) for edit conflict checks. */
  mtimeMs: number;
}

export interface FsWriteResult {
  relativePath: string;
  absolutePath: string;
  size: number;
  mtimeMs: number;
}

/** List directory under an added project root (relative path, "" = root). */
export async function fsListDir(projectPath: string, relative = "") {
  return invoke<FsEntry[]>("fs_list_dir", {
    projectPath,
    relative: relative || null,
  });
}

/** Read file under project root for preview (text or base64). */
export async function fsReadFile(projectPath: string, relative: string) {
  return invoke<FsReadResult>("fs_read_file", {
    projectPath,
    relative,
  });
}

/** Save UTF-8 text under project root. Pass mtime from last read to detect conflicts. */
export async function fsWriteFile(
  projectPath: string,
  relative: string,
  content: string,
  expectedMtimeMs?: number | null,
) {
  return invoke<FsWriteResult>("fs_write_file", {
    projectPath,
    relative,
    content,
    expectedMtimeMs: expectedMtimeMs ?? null,
  });
}

/** Save UTF-8 text to an absolute path open in the resource pane. */
export async function fsWriteAbsolute(
  path: string,
  content: string,
  expectedMtimeMs?: number | null,
) {
  return invoke<FsWriteResult>("fs_write_absolute", {
    path,
    content,
    expectedMtimeMs: expectedMtimeMs ?? null,
  });
}

/** Read absolute filesystem path for chat → resource pane preview. */
export async function fsReadAbsolute(path: string) {
  return invoke<FsReadResult>("fs_read_absolute", { path });
}

/**
 * Smart open for chat file cards: absolute path, project-relative, or
 * suffix search under project (e.g. `05-handoff/next.md` in a subfolder).
 */
export async function fsOpenPath(path: string, projectPath?: string | null) {
  return invoke<FsReadResult>("fs_open_path", {
    path,
    projectPath: projectPath ?? null,
  });
}

/** Remove project from app list only (no disk / session wipe). */
export async function projectRemove(id: string) {
  return invoke<ProjectRecord>("project_remove", { id });
}

/**
 * Point project at a new directory (folder moved/renamed).
 * Host re-checks is_dir and sets pathOk true.
 */
export async function projectRelocate(id: string, path: string) {
  return invoke<ProjectRecord>("project_relocate", { id, path });
}

export async function projectRename(id: string, name: string) {
  return invoke<ProjectRecord>("project_rename", { id, name });
}

export async function projectSetPinned(id: string, pinned: boolean) {
  return invoke<ProjectRecord>("project_set_pinned", { id, pinned });
}

export async function projectReveal(id: string) {
  return invoke("project_reveal", { id });
}

/** KeenCode 当前唯一的应用设置结构。 */
export interface AppSettings {
  /** 应用更新安装包的下载源偏好。 */
  appUpdateDownloadSource: AppUpdateDownloadSource;
  /** Windows WebView2 是否启用硬件加速。 */
  chromeHardwareAcceleration: boolean;
  /** 是否展示每轮全部思考片段。 */
  showFullThinking: boolean;
  /** 侧栏中由用户折叠的项目标识。 */
  sidebarCollapsedProjectIds: string[];
  /** 是否自动归档符合条件的旧任务。 */
  autoArchiveOldTasks: boolean;
  /** 自动归档前的未更新保留天数。 */
  archiveRetentionDays: number;
  /** 是否发送任务桌面通知。 */
  taskNotifications: boolean;
  /** 任务通知是否播放系统默认提示音。 */
  notificationSound: boolean;
  /** 是否阻止系统因用户空闲自动进入睡眠。 */
  keepComputerAwake: boolean;
  /** 是否生成并使用此电脑上的本地记忆。 */
  localMemories: boolean;
}

/** 当前界面允许局部更新的应用设置。 */
export type AppSettingsPatch = Partial<
  Pick<
    AppSettings,
    | "appUpdateDownloadSource"
    | "chromeHardwareAcceleration"
    | "showFullThinking"
    | "sidebarCollapsedProjectIds"
    | "autoArchiveOldTasks"
    | "archiveRetentionDays"
    | "taskNotifications"
    | "notificationSound"
    | "keepComputerAwake"
    | "localMemories"
  >
>;

export async function settingsGet() {
  return invoke<AppSettings>("settings_get");
}

export async function settingsSet(settings: AppSettingsPatch) {
  return invoke<AppSettings>("settings_set", { settings });
}

/** 读取当前设备唯一的全局用户自定义指令；首次使用时为空。 */
export async function customInstructionsGet(): Promise<string> {
  return invoke<string>("custom_instructions_get", {});
}

/** 校验并保存当前设备唯一的全局用户自定义指令。 */
export async function customInstructionsSet(
  instructions: string,
): Promise<string> {
  return invoke<string>("custom_instructions_set", { instructions });
}

export interface MemoryStatus {
  enabled: boolean;
  root: string;
  memoryCount: number;
  running: boolean;
}

export async function memoriesStatus() {
  return invoke<MemoryStatus>("memories_status");
}

export async function memoriesReset() {
  return invoke<void>("memories_reset");
}

// ── Skills / MCP / 插件 ───────────────────────────────────────────────────

/** KeenCode 当前唯一的 Skill 来源集合。 */
export type SkillSource = "user" | "project" | "plugin";

export interface SkillDto {
  /** Skill 稳定名称。 */
  name: string;
  /** Skill 用途说明。 */
  description: string;
  /** 当前 Skill 的唯一来源类型。 */
  source: SkillSource;
  /** Skill 主文件路径。 */
  path: string;
  /** 是否允许通过斜杠命令调用。 */
  userInvocable: boolean;
}

/** KeenCode 当前唯一的 MCP 传输类型。 */
export type McpTransport = "stdio" | "http";

export interface McpDto {
  /** MCP Server 稳定名称。 */
  name: string;
  /** MCP 传输类型。 */
  transport: McpTransport;
  /** MCP 命令或 URL。 */
  target: string | null;
  /** 唯一 MCP 配置中的启用状态。 */
  enabled: boolean;
}

/** Peri MCP 连接池初始化阶段。 */
export type McpRuntimeInitPhase = "pending" | "initializing" | "ready" | "failed";

/** Peri MCP Server 当前连接状态。 */
export type McpRuntimeStatus =
  | "connected"
  | "failed"
  | "disconnected"
  | "disabled"
  | "uninitialized";

/** Peri MCP Server 当前 OAuth 状态。 */
export type McpOAuthStatus = "none" | "authorized" | "needs_authorization";

/** Peri 运行时返回的单个 MCP Server 快照。 */
export interface McpRuntimeServer {
  /** MCP Server 稳定名称。 */
  name: string;
  /** 当前连接状态。 */
  status: McpRuntimeStatus;
  /** 当前传输类型。 */
  transport: string;
  /** 已发现的工具数量。 */
  toolsCount: number;
  /** 当前 OAuth 状态。 */
  oauthStatus: McpOAuthStatus;
  /** 当前连接失败原因；无错误时为空。 */
  error: string | null;
}

/** Peri MCP 连接池只读快照；读取不会启动网络连接或子进程。 */
export interface McpRuntimeSnapshot {
  /** 连接池初始化阶段。 */
  initPhase: McpRuntimeInitPhase;
  /** 当前已登记的 Server 运行态。 */
  servers: McpRuntimeServer[];
}

export interface SkillsListResult {
  /** 当前可用的 Skills。 */
  skills: SkillDto[];
}

/** 子智能体来源。 */
export type AgentSource = "global" | "builtin" | "plugin";

export interface AgentDto {
  /** 子智能体稳定标识。 */
  name: string;
  /** 主智能体用于判断委托时机的说明。 */
  description: string;
  /** 当前定义来自 KeenCode 全局目录还是内置运行时。 */
  source: AgentSource;
  /** 全局定义文件路径；内置子智能体没有外部路径。 */
  path: string | null;
  /** 全局子智能体的模型覆盖（"{provider_id}::{model}"）；null 表示跟随会话 Provider。 */
  model: string | null;
}

export interface AgentsListResult {
  /** 所有项目共享的全局与内置子智能体。 */
  agents: AgentDto[];
}

export interface InspectMcpResult {
  /** KeenCode 唯一 MCP 配置中的 Server。 */
  servers: McpDto[];
}

/** 查询 Peri MCP 当前运行态；查询本身不会触发初始化。 */
export async function mcpRuntimeList() {
  return invoke<McpRuntimeSnapshot>("mcp_list");
}

/** 显式启动指定 MCP Server 的 OAuth 授权。 */
export async function mcpOauthStart(serverName: string) {
  return invoke<{ success: boolean }>("mcp_oauth_start", { serverName });
}

/** 将手动取得的 OAuth 授权码与 state 回传给指定 MCP Server。 */
export async function mcpOauthCallback(
  serverName: string,
  code: string,
  state: string,
) {
  return invoke<{ success: boolean }>("mcp_oauth_callback", {
    serverName,
    code,
    state,
  });
}

/** 取消指定 MCP Server 尚未完成的 OAuth 授权。 */
export async function mcpOauthCancel(serverName: string) {
  return invoke<{ success: boolean }>("mcp_oauth_cancel", { serverName });
}

/** 设置一个 MCP Server 的启用状态；下一次任务会自动重连。 */
export async function extensionsSetMcp(name: string, enabled: boolean) {
  return invoke<void>("extensions_set_mcp", { name, enabled });
}

/** 批量启用当前列出的 MCP Server。 */
export async function extensionsEnableAllMcp(names: string[]) {
  return invoke<void>("extensions_enable_all_mcp", { names });
}

/** 列出用户级与可选项目级 Skills。 */
export async function skillsList(projectPath?: string | null) {
  return invoke<SkillsListResult>("skills_list", {
    projectPath: projectPath ?? null,
  });
}

/** 列出 KeenCode 全局与内置的子智能体。 */
export async function agentsList() {
  return invoke<AgentsListResult>("agents_list");
}

/** 创建子智能体时可勾选授权的工具目录。 */
export interface AgentToolCatalogResult {
  tools: string[];
}

/** 返回创建子智能体时可勾选授权的工具目录。 */
export async function agentsToolCatalog() {
  return invoke<AgentToolCatalogResult>("agents_tool_catalog");
}

/** 创建一个所有项目共享的全局子智能体定义。tools 传 null 表示继承主智能体的全部工具。 */
export async function agentCreate(input: {
  name: string;
  description: string;
  prompt: string;
  tools: string[] | null;
  maxTurns: number | null;
}) {
  return invoke<void>("agent_create", input);
}

/** 删除一个全局子智能体定义。 */
export async function agentRemove(name: string) {
  return invoke<void>("agent_remove", { name });
}

/** 更新全局子智能体的模型覆盖。model 传 null 表示清除覆盖、跟随会话 Provider。 */
export async function agentUpdate(name: string, model: string | null) {
  return invoke<void>("agent_update", { name, model });
}

/** 列出 KeenCode 唯一 MCP 配置中的 Server。 */
export async function inspectMcp() {
  return invoke<InspectMcpResult>("inspect_mcp");
}

// ── KeenCode 本地插件登记与 Skills 注入 ───────────────────────────────────────

/** 插件清单声明的 Skill 数量。 */
export interface PluginProvidesDto {
  /** Claude Commands 数量。 */
  commands?: number;
  /** 插件包含的 Skill 数量。 */
  skills: number;
  /** Claude Agents 数量。 */
  agents?: number;
  /** Hooks 声明数量。 */
  hooks?: number;
  /** MCP Server 数量。 */
  mcp?: number;
  /** LSP Server 数量；运行时在 KeenCode 启动时装配。 */
  lsp?: number;
}

export interface PluginDto {
  /** 插件稳定名称。 */
  name: string;
  /** 插件版本，未声明时为 null。 */
  version: string | null;
  /** 来源市场名称，直接路径安装时为 null。 */
  marketplace: string | null;
  /** 插件根目录的规范化绝对路径。 */
  path: string;
  /** KeenCode 插件登记中的启用状态。 */
  enabled: boolean;
  /** 从当前插件清单实时计算的组件信息。 */
  provides: PluginProvidesDto;
  /** hooks.json 中声明了但运行时无法识别的事件名（拼写错误或未实现事件）；运行时会静默跳过。 */
  unsupportedHooks?: string[];
}

export interface PluginsListResult {
  /** KeenCode 已登记且当前清单有效的插件。 */
  plugins: PluginDto[];
}

export interface PluginDetailsResult {
  name: string;
  details: string;
}

/** 列出 KeenCode 已登记的本地插件。 */
export async function pluginsList() {
  return invoke<PluginsListResult>("plugins_list");
}

/** 启用本地插件并刷新扩展配置。 */
export async function pluginEnable(name: string) {
  return invoke<void>("plugin_enable", { name });
}

/** 禁用本地插件并刷新扩展配置。 */
export async function pluginDisable(name: string) {
  return invoke<void>("plugin_disable", { name });
}

/** 移除本地插件登记，不删除用户的来源目录。 */
export async function pluginUninstall(name: string) {
  return invoke<void>("plugin_uninstall", { name });
}

/** 返回插件目录的安全组件摘要。 */
export async function pluginDetails(name: string) {
  return invoke<PluginDetailsResult>("plugin_details", { name });
}

export interface PluginUserConfigFieldDto {
  /** 配置字段稳定名称。 */
  name: string;
  /** Claude userConfig 声明的值类型。 */
  valueType: string;
  /** 设置界面展示标题。 */
  title: string | null;
  /** 设置界面展示说明。 */
  description: string | null;
  /** 是否必须填写。 */
  required: boolean;
  /** 是否写入敏感凭据存储；敏感值不会从后端返回。 */
  sensitive: boolean;
  /** 是否接收同类型值数组。 */
  multiple: boolean;
  /** 默认值；敏感字段不会返回默认值。 */
  default: unknown;
  /** 当前公开值；敏感字段始终为空。 */
  value: unknown;
  /** select 字段的候选值；旧后端未提供时为空。 */
  enumValues?: unknown[];
  /** 数字或文本/路径长度下限。 */
  min?: number | null;
  /** 数字或文本/路径长度上限。 */
  max?: number | null;
}

export interface PluginUserConfigResult {
  plugin: string;
  fields: PluginUserConfigFieldDto[];
}

/** 读取 Claude 插件 userConfig 定义与非敏感值。 */
export async function pluginUserConfigGet(name: string) {
  return invoke<PluginUserConfigResult>("plugin_user_config_get", { name });
}

/** 保存 Claude 插件 userConfig；敏感值由桌面后端单独保存。 */
export async function pluginUserConfigSet(
  name: string,
  values: Record<string, unknown>,
  replace = false,
) {
  return invoke<PluginUserConfigResult>("plugin_user_config_set", {
    name,
    values,
    replace,
  });
}

/** 从本地路径或已添加的本地市场登记插件。 */
export async function pluginInstall(source: string) {
  return invoke<void>("plugin_install", { source });
}

/** 从本地来源刷新一个插件，名称为空时刷新全部插件。 */
export async function pluginUpdate(name?: string | null) {
  const n = (name ?? "").trim();
  return invoke<void>("plugin_update", {
    name: n ? n : null,
  });
}

// ── 自定义供应商 ───────────────────────────────────────────────────────────

export interface CustomProvider {
  id: string;
  models: string[];
  baseUrl: string;
  name: string;
  apiBackend: string;
  /** 已保存的 API Key，供前端显示/隐藏查看；null 表示无认证。 */
  apiKey?: string | null;
  /** 每模型手工配置的上下文窗口（token）；空对象表示全部未配置。 */
  contextWindows?: Record<string, number>;
  /** 启用 1M 上下文的模型集合；空对象表示全部未启用。 */
  context1m?: Record<string, boolean>;
}

export interface ProvidersListResult {
  providers: CustomProvider[];
  defaultModel: string | null;
  activeProviderId: string | null;
}

export async function providersList() {
  return invoke<ProvidersListResult>("providers_list");
}

export async function providersUpsert(body: {
  id: string;
  models: string[];
  baseUrl: string;
  name?: string;
  apiKey?: string;
  apiBackend: string;
  contextWindows?: Record<string, number>;
  context1m?: Record<string, boolean>;
  createOnly: boolean;
}) {
  return invoke<ProvidersListResult>("providers_upsert", {
    id: body.id,
    models: body.models,
    baseUrl: body.baseUrl,
    name: body.name ?? null,
    apiKey: body.apiKey ?? null,
    apiBackend: body.apiBackend,
    contextWindows: body.contextWindows ?? {},
    context1m: body.context1m ?? {},
    createOnly: body.createOnly,
  });
}

export async function providersRemove(id: string) {
  return invoke<ProvidersListResult>("providers_remove", { id });
}

export async function providersSelectModel(providerId: string, modelId: string) {
  return invoke<ProvidersListResult>("providers_select_model", {
    providerId,
    modelId,
  });
}

export async function providersListModels(opts: {
  baseUrl: string;
  apiKey?: string;
  providerId?: string;
  apiBackend: string;
}) {
  return invoke<{
    models: Array<{
      id: string;
      ownedBy: string | null;
      contextWindow: number | null;
    }>;
  }>("providers_list_models", {
    baseUrl: opts.baseUrl,
    apiKey: opts.apiKey ?? null,
    providerId: opts.providerId ?? null,
    apiBackend: opts.apiBackend,
  });
}

/** 用于粗略费用估算的每百万 token 美元价格。 */
export interface ModelPrice {
  /** 每百万输入 token 的美元价格。 */
  inputPerMillion: number;
  /** 每百万输出 token 的美元价格。 */
  outputPerMillion: number;
  /** 每百万缓存读取 token 的美元价格。 */
  cacheReadPerMillion: number | null;
  /** 每百万缓存写入 token 的美元价格。 */
  cacheWritePerMillion: number | null;
}

/** 离散推理强度控制。 */
export interface ModelReasoningEffortControl {
  /** 推理控制类型。 */
  type: "effort";
  /** 按固定语义顺序排列的推理强度。 */
  values: string[];
}

/** 推理开关控制。 */
export interface ModelReasoningToggleControl {
  /** 推理控制类型。 */
  type: "toggle";
}

/** 推理 token 预算控制。 */
export interface ModelReasoningBudgetControl {
  /** 推理控制类型。 */
  type: "budget_tokens";
  /** 最小推理 token 数。 */
  min: number | null;
  /** 最大推理 token 数。 */
  max: number | null;
}

/** 模型支持的任一推理控制形式。 */
export type ModelReasoningControl =
  | ModelReasoningEffortControl
  | ModelReasoningToggleControl
  | ModelReasoningBudgetControl;

/** 模型推理支持与控制信息。 */
export interface ModelReasoningInfo {
  /** 当前目录是否明确声明支持推理。 */
  supported: boolean;
  /** 当前目录声明的推理控制形式。 */
  controls: ModelReasoningControl[];
  /** 当前目录声明的默认推理强度。 */
  defaultEffort: string | null;
  /** 当前目录是否声明推理不可关闭。 */
  mandatory: boolean | null;
}

/** 单个模型字段采用的数据源。 */
export interface ModelMetadataFieldSource {
  /** 远端目录稳定标识。 */
  catalog: string;
  /** 远端目录中实际命中的模型标识。 */
  matchedModelId: string;
}

/** 模型元数据各字段的来源。 */
export interface ModelMetadataSources {
  /** 价格字段来源。 */
  price: ModelMetadataFieldSource | null;
  /** 上下文窗口字段来源。 */
  contextWindow: ModelMetadataFieldSource | null;
  /** 最大输出 token 字段来源。 */
  maxOutputTokens: ModelMetadataFieldSource | null;
  /** 推理信息字段来源。 */
  reasoning: ModelMetadataFieldSource | null;
}

/** 按模型标识缓存的价格、上下文与推理元数据。 */
export interface ModelMetadata {
  /** 用户配置的原始模型标识。 */
  modelId: string;
  /** 用于粗略费用统计的价格。 */
  price: ModelPrice | null;
  /** 模型上下文窗口 token 数。 */
  contextWindow: number | null;
  /** 模型最大输出 token 数。 */
  maxOutputTokens: number | null;
  /** 模型推理支持与控制信息；空值表示未知。 */
  reasoning: ModelReasoningInfo | null;
  /** 每个字段实际采用的数据源。 */
  sources: ModelMetadataSources;
  /** 最近一次成功解析目录的 Unix 秒时间戳。 */
  updatedAt: number;
}

/** 按模型标识读取缓存，必要时按固定数据源顺序刷新。 */
export async function modelMetadataGet(modelId: string) {
  return invoke<ModelMetadata>("model_metadata_get", { modelId });
}

/** 订阅 Tauri 事件；浏览器预览不建立空事件源。 */
export async function listen<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<() => void> {
  if (!isTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  const un = await listen<T>(event, (e) => handler(e.payload));
  return un;
}

export type GitWorktreeGcResult = {
  /** 是否只预览清理。 */
  dryRun: boolean;
  /** 是否立即过期所有可清理记录。 */
  force: boolean;
  /** 清理或预计清理的记录数量。 */
  prunedCount: number;
  /** 执行前标记为可清理的工作树路径。 */
  prunable: string[];
  /** Git 标准输出。 */
  stdout: string;
  /** Git 标准错误。 */
  stderr: string;
  /** 合并后的可读输出。 */
  output: string;
};

/** 清理或预览清理指定仓库的无效 worktree 记录。 */
export async function gitWorktreeGc(opts: {
  /** 项目绝对路径。 */
  projectPath: string;
  /** 是否只预览。 */
  dryRun: boolean;
  /** 是否立即过期所有可清理记录。 */
  force: boolean;
  /** 可选的 Git 过期表达式。 */
  expire?: string | null;
}): Promise<GitWorktreeGcResult> {
  return invoke<GitWorktreeGcResult>("git_worktree_gc", {
    projectPath: opts.projectPath,
    dryRun: opts.dryRun,
    force: opts.force,
    expire: opts.expire ?? null,
  });
}

/** 本地插件市场来源。 */
export interface MarketplaceSourceDto {
  /** 市场稳定名称。 */
  name: string;
  /** 本地市场根目录。 */
  path: string;
}

/** 本地市场中可安装的插件。 */
export interface AvailablePluginDto {
  /** 插件稳定名称。 */
  name: string;
  /** 插件来源市场。 */
  marketplace: string;
  /** 插件说明。 */
  description: string | null;
  /** 插件版本。 */
  version: string | null;
  /** 插件包含的 Skill 数量。 */
  skillCount: number;
}

export type MarketplaceAvailableResult = {
  /** 当前所有本地市场中尚未安装的插件。 */
  plugins: AvailablePluginDto[];
};

/** 列出 KeenCode 管理的所有本地插件市场。 */
export async function marketplaceList() {
  return invoke<MarketplaceSourceDto[]>("marketplace_list");
}

/** 列出本地市场中尚未安装的插件。 */
export async function marketplaceAvailable() {
  return invoke<MarketplaceAvailableResult>("marketplace_available");
}

/** 添加一个本地插件市场目录或清单。 */
export async function marketplaceAdd(source: string) {
  return invoke<void>("marketplace_add", { source });
}

/** 按稳定名称移除一个本地市场记录。 */
export async function marketplaceRemove(name: string) {
  return invoke<void>("marketplace_remove", { name });
}

/** 重新校验一个或全部本地市场清单。 */
export async function marketplaceUpdate(name?: string | null) {
  return invoke<void>("marketplace_update", {
    name: name ?? null,
  });
}

export type McpDoctorReport = {
  /** 所有已列举 Server 是否健康。 */
  ok: boolean;
  /** MCP Server 检查明细。 */
  servers: Array<{
    /** MCP Server 名称。 */
    name: string;
    /** 所有检查是否通过。 */
    healthy: boolean;
    /** MCP 传输类型。 */
    transport: McpTransport;
    /** stdio 命令或远端地址。 */
    target: string | null;
    /** 不包含环境变量值的检查列表。 */
    checks: Array<{
      /** 检查项目名称。 */
      label: string;
      /** 检查是否通过。 */
      passed: boolean;
      /** 检查结果说明。 */
      detail: string;
    }>;
  }>;
  /** KeenCode 唯一 MCP 配置源状态。 */
  sources: Array<{
    /** 配置文件绝对路径。 */
    path: string;
    /** 当前配置文件状态。 */
    status: "configured" | "missing";
    /** 当前配置中的 Server 数量。 */
    serverCount: number;
  }>;
  /** MCP Doctor 汇总计数。 */
  summary: {
    /** 健康 Server 数量。 */
    healthy: number;
    /** 不健康 Server 数量。 */
    unhealthy: number;
    /** Server 总数。 */
    total: number;
  };
  /** 空列表或指定名称不存在时的说明。 */
  rawText: string | null;
};

export async function mcpAdd(opts: {
  name: string;
  command: string;
  args?: string[];
  env?: Record<string, string>;
}) {
  return invoke<void>("mcp_add", opts);
}

export async function mcpRemove(name: string) {
  return invoke<void>("mcp_remove", { name });
}

/** 诊断全部 MCP Server，或按名称聚焦一个 Server。 */
export async function mcpDoctor(focus?: string | null) {
  return invoke<McpDoctorReport>("mcp_doctor", {
    focus: focus ?? null,
  });
}

// ── 本地请求记录与用量统计 ───────────────────────────────────────────────────

/** 一次 LLM 请求的记录（serde camelCase 与后端 RequestRecord 对齐）。 */
export interface RequestRecord {
  /** 记录稳定标识。 */
  id: string;
  /** 所属会话标识。 */
  sessionId: string;
  /** 请求模型标识。 */
  model: string;
  /** 请求模式："sync" | "async"。 */
  requestMode: string;
  /** 请求发起时间（Unix 毫秒）。 */
  requestedAtMs: number;
  /** 请求耗时（毫秒）。 */
  durationMs: number;
  /** 原始请求体。 */
  request: unknown;
  /** 响应文本。 */
  response: string;
  /** 输入 token 数。 */
  inputTokens: number;
  /** 输出 token 数。 */
  outputTokens: number;
  /** token 数是否为估算值。 */
  estimated: boolean;
  /** 供应商侧请求标识；未知时为 null。 */
  providerRequestId: string | null;
}

/** 单个模型的用量统计。 */
export interface ModelUsageStat {
  /** 模型标识。 */
  model: string;
  /** 请求次数。 */
  requests: number;
  /** 输入 token 数。 */
  inputTokens: number;
  /** 输出 token 数。 */
  outputTokens: number;
  /** 总 token 数。 */
  totalTokens: number;
}

/** 单日用量统计。 */
export interface DailyUsageStat {
  /** 本地时区日期（YYYY-MM-DD）。 */
  date: string;
  /** 当日请求次数。 */
  requests: number;
  /** 当日总 token 数。 */
  totalTokens: number;
  /** 按模型统计的当日 token 数。 */
  modelTokens: Record<string, number>;
}

/** 全量用量统计。 */
export interface UsageStats {
  /** 全部请求次数。 */
  totalRequests: number;
  /** 全部 token 数。 */
  totalTokens: number;
  /** 按模型聚合的用量。 */
  models: ModelUsageStat[];
  /** 按日聚合的用量。 */
  days: DailyUsageStat[];
}

/** 返回最近的请求记录（默认 200 条，最多 1000 条）。 */
export async function requestRecordsList(
  limit?: number | null,
): Promise<RequestRecord[]> {
  return invoke<RequestRecord[]>("request_records_list", {
    limit: limit ?? null,
  });
}

/** 返回按模型与日期聚合的用量统计。 */
export async function usageStatsGet(): Promise<UsageStats> {
  return invoke<UsageStats>("usage_stats_get");
}
