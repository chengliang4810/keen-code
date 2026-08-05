/**
 * Mid-stream tool activity — plain one-line text (Codex-style).
 *
 * Rules:
 * - Only the latest **running** tool is shown
 * - Multiple tools replace the same line (no stack)
 * - Line sits in the stream (after current reply / at live edge)
 * - Hidden when no running tool (content can resume without chrome)
 * - Historical tool_step rows prefer assistant timeline segments (not a bottom dump)
 * - Failures surface as quiet red marks on timeline tool rows
 */

import type { Locale } from "@/i18n";
import type { ChatMessage } from "@/lib/session";
import { toolStepDisplayTitle } from "@/lib/session";

export {
  isToolStepMessage,
  isFailedToolStepMessage,
  pickLatestTurnTool,
  pickRunningTurnTool,
  toolStepDisplayTitle,
} from "@/lib/session";

/**
 * Mid-stream tool status — plain call text only (no "tool" chrome).
 * Hidden when there is no meaningful title yet.
 */
export function LiveToolText({
  message,
  locale: _locale,
}: {
  message: ChatMessage;
  locale: Locale;
}) {
  const title = toolStepDisplayTitle(message);
  if (!title) return null;

  return (
    <div
      className="lobe-chat-tool-text"
      role="status"
      aria-live="polite"
      data-tool-id={message.toolCallId}
      title={message.toolDetail || message.toolPath || title}
    >
      {title}
    </div>
  );
}
