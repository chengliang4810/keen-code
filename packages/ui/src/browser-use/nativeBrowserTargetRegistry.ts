import type { NativeBrowserTarget } from "@zcode/shared";

const targets = new Map<string, NativeBrowserTarget>();

function sameTarget(left: NativeBrowserTarget, right: NativeBrowserTarget): boolean {
  return (
    left.tabId === right.tabId &&
    left.generation === right.generation &&
    left.owner.workspaceKey === right.owner.workspaceKey &&
    left.owner.sessionId === right.owner.sessionId &&
    left.owner.browserGeneration === right.owner.browserGeneration
  );
}

/** 仅供同一 renderer 的 tab 关闭协调读取；Rust 仍以自身 registry 作为最终授权边界。 */
export function registerNativeBrowserTarget(target: NativeBrowserTarget): void {
  targets.set(target.tabId, target);
}

export function getNativeBrowserTarget(tabId: string): NativeBrowserTarget | null {
  return targets.get(tabId) ?? null;
}

export function unregisterNativeBrowserTarget(target: NativeBrowserTarget): void {
  const current = targets.get(target.tabId);
  if (current && sameTarget(current, target)) {
    targets.delete(target.tabId);
  }
}
