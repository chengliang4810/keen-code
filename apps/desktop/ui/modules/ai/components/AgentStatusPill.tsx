import { useTranslation } from "@/modules/i18n";
import { Spinner } from "@/components/localized/spinner";
import { cn } from "@/lib/utils";
import { AlertCircleIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { useChatStore, type AgentMeta } from "../store/chatStore";
import { agentStepLabel } from "../lib/agentStepLabel";

type Props = {
  onClick: () => void;
};

export function AgentStatusPill({ onClick }: Props) {
  const tr = useTranslation();
  const meta = useChatStore((s) => s.agentMeta);

  // awaiting-approval is surfaced by the notification + auto-opened mini window.
  if (meta.status === "awaiting-approval") return null;
  if (meta.status === "idle" && !meta.error) return null;

  const { tone, icon, label } = describe(meta, tr);

  return (
    <button
      key={`${meta.status}:${label}`}
      type="button"
      onClick={onClick}
      className={cn(
        "flex h-6 items-center gap-1.5 rounded-md border px-1.5 text-ui-sm transition-colors",
        "animate-in fade-in-0 slide-in-from-top-1 duration-150 ease-out",
        tone,
      )}
      title={tr("Open AI log")}
    >
      {icon}
      <span className="max-w-[180px] truncate">
        {meta.error ? label : agentStepLabel(label, tr)}
      </span>
    </button>
  );
}

function describe(
  meta: AgentMeta,
  tr: ReturnType<typeof useTranslation>,
): {
  tone: string;
  icon: React.ReactNode;
  label: string;
} {
  if (meta.status === "error") {
    return {
      tone: "border-destructive/40 bg-destructive/10 text-destructive hover:bg-destructive/15",
      icon: (
        <HugeiconsIcon icon={AlertCircleIcon} size={12} strokeWidth={1.75} />
      ),
      label: meta.error ?? tr("Error"),
    };
  }
  // thinking | streaming
  return {
    tone: "border-border/60 bg-card text-muted-foreground hover:text-foreground",
    icon: <Spinner className="size-3" />,
    label: meta.step ?? tr("Thinking…"),
  };
}
