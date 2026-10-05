import type { ReactNode } from "react";

/** 远程账号/设置同步引导已移除；本地工作区直接进入 Root。 */
export function OnboardingDialog(_props: {
  workspacePath?: string;
  workspaceIdentity?: string;
  isDesktop?: boolean;
}): ReactNode {
  return null;
}

export function shouldAutoScanOnboardingSessions(): boolean {
  return false;
}
