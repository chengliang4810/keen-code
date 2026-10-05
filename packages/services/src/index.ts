// Descriptors & collection (browser-safe)
export { type ServiceDescriptor, createServiceDescriptor } from "./descriptors.js";
export { ServiceCollection } from "./collection.js";
export {
  IModelSelectionService,
  IProviderSettingsService,
  type ModelSelectionView,
  type ModelSelectionViewInput,
  type ProviderSettingsProviderView,
  type ProviderSettingsView,
} from "./model-provider/providerFacadeServices.js";
export { IProviderProvisioningTargetService } from "./model-provider/providerProvisioning.js";
export {
  collectServiceMemoryDiagnostics,
  memoryDiagnosticsRegistry,
  registerMemoryDiagnosticsProvider,
} from "./memoryDiagnostics.js";

// Accessor
export type { IServiceAccessor } from "./accessor.js";
// File service — IFileService is both a type (interface) and value (descriptor)
export { IFileService } from "./file/file.js";
export { IMediaPreviewService } from "./media-preview/mediaPreview.js";
export type { MediaPreviewPreparation } from "./media-preview/mediaPreview.js";

// Git service — IGitService is both a type (interface) and value (descriptor)
export { IGitService } from "./git/git.js";
export { IGitCheckpointService } from "./git/gitCheckpoint.js";

// System service — ISystemService is both a type (interface) and value (descriptor)
export { ISystemService } from "./system/system.js";

// Terminal service — ITerminalService is both a type (interface) and value (descriptor)
export { ITerminalService } from "./terminal/terminal.js";

// Setting service — ISettingService is both a type (interface) and value (descriptor)
export { ISettingService } from "./setting/setting.js";

// Credential service — ICredentialService is both a type (interface) and value (descriptor)
export { ICredentialService } from "./credential/credential.js";

// Broadcast service — IBroadcastService is both a type (interface) and value (descriptor)
export { IBroadcastService } from "./broadcast/broadcast.js";

// Onboarding 完成记录服务（本地持久化，后续上传服务器）
export { IOnboardingRecordService } from "./onboarding/onboardingRecord.js";
export type {
  CreateOnboardingRecordServiceOptions,
  OnboardingRecordServiceFactory,
} from "./onboarding/onboardingRecord.js";
// 这里只能导出 descriptor 和类型。根 index 会被 renderer 经 value import 拉进浏览器包，
// 不应把本地持久化工厂或其他宿主实现带入浏览器模块图，避免模块加载阶段触发宿主依赖。
// 工厂函数由桌面能力层通过明确的 RPC/宿主边界提供。
export type {
  BroadcastClaimAcquireResult,
  BroadcastClaimLease,
  BroadcastMessage,
} from "./broadcast/broadcast.js";

// ZCode task wrapper service — task 列表/置顶/归档等 app 侧包装状态入口。
export { IZCodeTaskService } from "./session/zcodeTaskService.js";
export type {
  ZCodeArchivedTaskDeletionResult,
  ZCodeModelTrajectory,
  ZCodeModelTrajectoryCallSource,
  ZCodeModelTrajectoryCallSourceKind,
  ZCodeModelTrajectoryContentPart,
  ZCodeModelTrajectoryMessage,
  ZCodeModelTrajectoryRecord,
  ZCodeModelTrajectoryUsage,
  ZCodeTaskListKind,
  ZCodeTaskListQuery,
  ZCodeTaskListResult,
  ZCodeTaskListSortBy,
  ZCodeTaskListWorkspaceScope,
  ZCodeTaskReadyOutcome,
  ZCodeGroupedTaskRef,
  ZCodeGroupedTaskView,
  ZCodeGroupedTaskViewNode,
  ZCodeGroupedTaskViewOrderInput,
  ZCodeGroupedTaskViewQuery,
  ZCodeGroupedTaskViewStructure,
  ZCodeGroupedTaskViewStructureMember,
  ZCodeGroupedTaskViewStructureTopOrder,
  ZCodeGroupedTaskViewTopLevelNodeRef,
  ZCodeTaskGroup,
  ZCodeTaskGroupColor,
} from "./session/zcodeTaskService.js";
export type { ZCodeTaskListItem } from "./session/zcodeTaskListTypes.js";

export { IWindowControllerService } from "./window-controller/windowController.js";
export type {
  WindowHostControllerFrame,
  WindowHostControllerMutation,
  WindowHostControllerTaskListItem,
  WindowHostControllerTaskListResult,
} from "./window-controller/windowController.js";

// ZCode agent service — IZCodeAgentService is both a type (interface) and value (descriptor)
export {
  IZCodeAgentService,
  type ZCodeAgentLocalRuntimeChildProcesses,
  ZCODE_AGENT_RUNTIME_UNAVAILABLE_CODE,
} from "./zcode-agent/zcodeAgent.js";
export {
  isZCodeAgentMcpStatusModeUnsupportedError,
  ZCODE_AGENT_MCP_STATUS_MODE_UNSUPPORTED_ERROR_CODE,
  ZCodeAgentMcpStatusModeUnsupportedError,
} from "./zcode-agent/zcodeAgentErrors.js";
export {
  createZCodeAgentConnectionScope,
  readTrustedZCodeAgentV4Connection,
} from "./zcode-agent/zcodeAgentConnectionScope.js";
export type {
  ZCodeAgentConnectionScope,
  ZCodeAgentV4ClientMode,
  ZCodeAgentV4ConnectionContext,
} from "./zcode-agent/zcodeAgentConnectionScope.js";
export type {
  ZCodeAgentAttachmentBeginParams,
  ZCodeAgentAttachmentChunkParams,
  ZCodeAgentAttachmentTerminalParams,
  ZCodeAgentCreateSessionParams,
  ZCodeAgentInitializeResult,
  ZCodeAgentStorageStartupSnapshot,
  ZCodeAgentRuntimeLifecycleEvent,
  ZCodeAgentRuntimePolicy,
  ZCodeAgentReadSessionParams,
  ZCodeAgentResumeSessionParams,
  ZCodeAgentRunAutomationNowResult,
  ZCodeAgentSavedWorkflowTarget,
  ZCodeAgentSendPromptParams,
  ZCodeAgentServiceEvent,
  ZCodeAgentSessionSubscribeParams,
  ZCodeAgentSessionTarget,
  ZCodeAgentSetModeParams,
  ZCodeAgentSetModelParams,
  ZCodeAgentSetThoughtLevelParams,
  ZCodeAgentWorkspaceTarget,
} from "./zcode-agent/zcodeAgent.js";

// ZCode session service — app-facing session facade without ZCode Agent naming.
export { IZCodeSessionService } from "./zcode-session/zcodeSession.js";
export type {
  ZCodeSessionCreateParams,
  ZCodeSessionEventsParams,
  ZCodeSessionInitializeResult,
  ZCodeSessionListParams,
  ZCodeSessionMessagesParams,
  ZCodeSessionReadParams,
  ZCodeSessionResumeParams,
  ZCodeSessionServiceEvent,
  ZCodeSessionSetModeParams,
  ZCodeSessionSetModelParams,
  ZCodeSessionSetThoughtLevelParams,
  ZCodeSessionSubscribeParams,
  ZCodeTaskTarget,
  ZCodeSessionWorkspaceTarget,
} from "./zcode-session/zcodeSession.js";

// Hooks service — IHooksService is both a type (interface) and value (descriptor).
export { IHooksService } from "./hooks/hooks.js";

// Memory service — IMemoryService is both a type (interface) and value (descriptor).
export {
  IMemoryService,
  PROJECT_MEMORY_FILE_CHANGED_ERROR_CODE,
  PROJECT_MEMORY_PREVIEW_LIMIT_EXCEEDED_ERROR_CODE,
} from "./memory/memory.js";
export type { ProjectMemoryFileSummary, ProjectMemoryWorkspaceSummary } from "./memory/memory.js";

export type { SessionRealtimePort } from "./session/sessionRealtimePort.js";

// FileWatcher service — IFileWatcherService is both a type (interface) and value (descriptor)
export { IFileWatcherService } from "./fileWatcher/fileWatcher.js";

export { isValidCronExpr } from "./session/automationCronValidation.js";

// 本地对话生命周期遥测仅订阅当前 Agent RPC 事件，不包含账户、支付或云端上传。
export {
  createConversationTelemetryService,
  type IConversationTelemetryService,
  type ConversationTelemetryWorkspaceTarget,
} from "./conversation-telemetry/conversationTelemetry.js";

// Skills service — ISkillsService is both a type (interface) and value (descriptor)
export { ISkillsService } from "./skills/skills.js";
export { ISkillSyncService } from "./skill-sync/skillSync.js";
export { IMcpSyncService } from "./mcp-sync/mcpSync.js";
export { IPluginSyncService } from "./plugin-sync/pluginSync.js";
// Plugins service — IPluginsService is both a type (interface) and value (descriptor)
export { IPluginsService } from "./plugins/plugins.js";
// 设置页插件管理薄服务（UI 平台能力面不再直触 zcodeAgentService）
export { IPluginManagementService } from "./plugins/pluginManagement.js";

// Subagents service — ISubagentsService is both a type (interface) and value (descriptor)
export { ISubagentsService } from "./subagents/subagents.js";

// Commands service — ICommandsService is both a type (interface) and value (descriptor)
export { ICommandsService } from "./commands/commands.js";

export { IPromptAttachmentTransferService } from "./prompt-attachment-transfer/promptAttachmentTransfer.js";
export type {
  PromptAttachmentStageParams,
  PromptAttachmentStageResult,
  PromptAttachmentTransferPhase,
  PromptAttachmentTransferProgress,
} from "./prompt-attachment-transfer/promptAttachmentTransfer.js";
