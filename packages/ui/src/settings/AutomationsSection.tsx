import { useEffect, useState } from "react";
import type {
  SavedWorkflowLaunchTarget,
  SavedWorkflowProjectTarget,
  SavedWorkflowsOpenArtifactParams,
  SavedWorkflowsOpenRunParams,
  SavedWorkflowsOpenTarget,
} from "@/settings/saved-workflows/SavedWorkflowsSection.js";
import { SavedWorkflowsSection } from "@/settings/saved-workflows/SavedWorkflowsSection.js";
import type { AutomationsNavigationTab } from "@/lib/taskNavigationHistory.js";
import {
  AutomationsPageTitle,
  type AutomationsPageTab,
} from "@/settings/saved-workflows/AutomationsPageTitleSwitch.js";
import { LocalAutomationsSection } from "@/settings/LocalAutomationsSection.js";

export const AUTOMATIONS_TOAST_ANCHOR_ID = "automations-main-toast-anchor";

interface AutomationsSectionProps {
  workspacePath?: string | null;
  workspaceIdentity?: string;
  onCreateViaChat?: (prompt: string, target?: SavedWorkflowProjectTarget) => void;
  openAutomationId?: string | null;
  openAutomationTab?: AutomationsNavigationTab | null;
  onOpenAutomationConsumed?: () => void;
  onNavigateToLaunchedRun?: (target: SavedWorkflowLaunchTarget, sessionId: string) => void;
  onOpenWorkflowRun?: (params: SavedWorkflowsOpenRunParams) => void;
  onOpenWorkflowArtifact?: (params: SavedWorkflowsOpenArtifactParams) => void;
  openWorkflow?: SavedWorkflowsOpenTarget | null;
  onOpenWorkflowConsumed?: () => void;
  onOpenSession?: (params: {
    sessionId: string;
    workspacePath: string;
    workspaceIdentity?: string;
  }) => void;
}

/**
 * 本地自动化与工作流中枢。自动化使用 Rust 本地台账和调度器；工作流使用 Rust
 * workflow store。两者共享页面标题，但不共享持久化模型，避免把定时任务降级成聊天草稿。
 */
export function AutomationsSection({
  workspacePath,
  workspaceIdentity,
  onCreateViaChat,
  onNavigateToLaunchedRun,
  onOpenWorkflowRun,
  onOpenWorkflowArtifact,
  openWorkflow,
  onOpenWorkflowConsumed,
  openAutomationId,
  openAutomationTab,
  onOpenAutomationConsumed,
  onOpenSession,
}: AutomationsSectionProps) {
  const [pageTab, setPageTab] = useState<AutomationsPageTab>(
    openAutomationTab && openAutomationTab !== "workflow" ? "automation" : "workflow",
  );
  const onCreate = onCreateViaChat
    ? (prompt: string, target: SavedWorkflowProjectTarget) => onCreateViaChat(prompt, target)
    : undefined;

  // 历史导航可能直接打开某条 automation；先切到自动化标签，再让子页按 ID 定位。
  // workflow 深链仍由 SavedWorkflowsSection 自己消费。
  useEffect(() => {
    if (openAutomationId && openAutomationTab !== "workflow") {
      setPageTab("automation");
    }
  }, [openAutomationId, openAutomationTab]);

  return (
    <div className="flex min-h-0 w-full flex-1 flex-col">
      {pageTab === "automation" ? (
        <LocalAutomationsSection
          header={
            <AutomationsPageTitle
              workflowTabEnabled
              value={pageTab}
              onValueChange={setPageTab}
            />
          }
          workspacePath={workspacePath}
          workspaceIdentity={workspaceIdentity}
          openAutomationId={openAutomationId}
          onOpenAutomationConsumed={onOpenAutomationConsumed}
          onOpenSession={onOpenSession}
        />
      ) : (
        <SavedWorkflowsSection
          header={
            <AutomationsPageTitle
              workflowTabEnabled
              value={pageTab}
              onValueChange={setPageTab}
            />
          }
          workspacePath={workspacePath}
          workspaceIdentity={workspaceIdentity}
          onNavigateToLaunchedRun={onNavigateToLaunchedRun}
          onCreateViaChat={onCreate}
          onOpenWorkflowRun={onOpenWorkflowRun}
          onOpenWorkflowArtifact={onOpenWorkflowArtifact}
          openWorkflow={openWorkflow}
          onOpenWorkflowConsumed={onOpenWorkflowConsumed}
        />
      )}
    </div>
  );
}
