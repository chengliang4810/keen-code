import { useTranslation } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import {
  Cancel01Icon,
  Edit02Icon,
  FileEditIcon,
  FilePlusIcon,
  FolderAddIcon,
  TerminalIcon,
  Tick02Icon,
  ToolsIcon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import type { ToolUIPart } from "ai";
import { memo, useState } from "react";
import { ConversationToolDiffPreview } from "@/modules/ai/components/ConversationToolDiffPreview";
import { conversationToolDiff } from "@/modules/ai/lib/conversationToolPresentation";

type Props = {
  part: Extract<ToolUIPart, { state: "approval-requested" }>;
  toolName: string;
  onRespond: (approved: boolean) => void | PromiseLike<void>;
};

const TOOL_META: Record<string, { label: string; icon: typeof FilePlusIcon }> =
  {
    write_file: { label: "Write file", icon: FilePlusIcon },
    edit: { label: "Edit file", icon: FileEditIcon },
    multi_edit: { label: "Edit file (batch)", icon: Edit02Icon },
    create_directory: { label: "Create directory", icon: FolderAddIcon },
    bash_run: { label: "Run shell command", icon: TerminalIcon },
    bash_background: { label: "Spawn background process", icon: TerminalIcon },
  };

function AiToolApprovalImpl({ part, toolName, onRespond }: Props) {
  const tr = useTranslation();
  const meta = TOOL_META[toolName];
  const label = meta?.label ?? toolName;
  const Icon = meta?.icon ?? ToolsIcon;
  const input = part.input as Record<string, unknown>;
  const [responding, setResponding] = useState(false);
  const [error, setError] = useState("");
  const respond = async (approved: boolean) => {
    if (responding) return;
    setResponding(true);
    setError("");
    try {
      await onRespond(approved);
    } catch (e) {
      setError(String(e));
      setResponding(false);
    }
  };

  return (
    <div className="rounded-lg border border-border bg-card shadow-sm">
      <div className="flex items-center gap-2 border-b border-border/60 px-3 py-2">
        <span className="size-1.5 shrink-0 rounded-full bg-amber-500 animate-pulse" />
        <HugeiconsIcon
          icon={Icon}
          size={13}
          strokeWidth={1.75}
          className="shrink-0 text-muted-foreground"
        />
        <span className="text-ui-base font-medium text-foreground">
          {tr(label)}
        </span>
        <span className="ml-auto text-ui-xs text-muted-foreground">
          {tr(responding ? "Processing…" : "needs approval")}
        </span>
      </div>

      <div className="px-3 py-2.5">
        <PreviewBlock toolName={toolName} input={input} />
      </div>

      <div className="flex items-center justify-end gap-1.5 border-t border-border/60 px-3 py-2">
        <Button
          size="sm"
          variant="ghost"
          onClick={() => void respond(false)}
          disabled={responding}
          className="h-7 gap-1.5 text-ui-base"
        >
          <HugeiconsIcon icon={Cancel01Icon} size={12} strokeWidth={2} />
          {tr("Deny")}
        </Button>
        <Button
          size="sm"
          variant="default"
          onClick={() => void respond(true)}
          disabled={responding}
          className="h-7 gap-1.5 text-ui-base"
        >
          <HugeiconsIcon icon={Tick02Icon} size={12} strokeWidth={2} />
          {tr("Approve")}
        </Button>
      </div>
      {error && (
        <p role="alert" className="px-3 pb-2 text-ui-sm text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}

export const AiToolApproval = memo(AiToolApprovalImpl, (a, b) => {
  // 审批参数已经冻结，后续文本 token 不应使整张卡片重复渲染。
  return (
    a.toolName === b.toolName &&
    a.part.approval.id === b.part.approval.id &&
    a.onRespond === b.onRespond
  );
});

function PreviewBlock({
  toolName,
  input,
}: {
  toolName: string;
  input: Record<string, unknown>;
}) {
  const tr = useTranslation();
  if (toolName === "bash_run" || toolName === "bash_background") {
    const cwd = typeof input.cwd === "string" ? input.cwd : null;
    return (
      <div className="space-y-1.5">
        {cwd && (
          <div className="font-mono text-ui-sm text-muted-foreground">
            {cwd}
          </div>
        )}
        <pre
          className={cn(
            "max-h-40 overflow-auto rounded-md bg-muted/60 p-2 font-mono text-ui-base leading-relaxed",
          )}
        >
          {String(input.command ?? "")}
        </pre>
      </div>
    );
  }
  // 审批只展示已冻结的提交内容；原生工具同样可在主会话里完整审阅。
  if (["write_file", "edit", "multi_edit"].includes(toolName)) {
    const preview = conversationToolDiff({ name: toolName, input });
    return (
      <div className="space-y-2 text-ui-base">
        <div className="break-all font-mono text-muted-foreground">
          {String(input.path ?? input.file_path ?? "")}
        </div>
        <div className="text-ui-sm text-muted-foreground/80">
          {tr("Submitted changes")} · −{preview.removed} / +{preview.added}
          {input.replace_all ? ` ${tr("· replace all")}` : ""}
        </div>
        {preview.lines.length > 0 && (
          <ConversationToolDiffPreview preview={preview} />
        )}
      </div>
    );
  }
  if (toolName === "create_directory") {
    return (
      <div className="font-mono text-ui-base text-muted-foreground">
        {String(input.path ?? "")}
      </div>
    );
  }
  return (
    <pre className="overflow-auto rounded-md bg-muted/60 p-2 font-mono text-ui-base leading-relaxed">
      {JSON.stringify(input, null, 2)}
    </pre>
  );
}
