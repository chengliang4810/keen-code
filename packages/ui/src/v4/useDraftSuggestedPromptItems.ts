import { useMemo } from "react";
import type { DraftSuggestedPromptItem } from "@/v4/draftSuggestedPromptItems.js";

export function useDraftSuggestedPromptItems(_options?: {
  clientScenesService?: unknown;
  rpcReady?: boolean;
  workspaceKey?: string;
}): DraftSuggestedPromptItem[] {
  return useMemo(() => [], []);
}
