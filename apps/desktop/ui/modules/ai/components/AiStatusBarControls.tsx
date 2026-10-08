import { useTranslation } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { fmtShortcut, MOD_KEY } from "@/lib/platform";
import { cn } from "@/lib/utils";
import {
  ArrowUpIcon,
  Message01Icon,
  StopCircleIcon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { useComposer } from "@/modules/ai/lib/composer";
import { ComposerActionMenu } from "@/modules/ai/components/ComposerActionMenu";
import { useChatStore } from "@/modules/ai/store/chatStore";
import { ComposerPermissions } from "@/modules/ai/components/ComposerPermissions";
import { AgentSwitcher } from "@/modules/ai/components/AgentSwitcher";
import {
  ComposerModelSelector,
  ComposerReasoningSelector,
} from "@/modules/ai/components/ComposerModelSelector";

export function AiOpenButton({ onOpen }: { onOpen: () => void }) {
  const tr = useTranslation();
  return (
    <button
      type="button"
      onClick={onOpen}
      className={cn(
        "flex h-6 items-center gap-1.5 rounded-md border border-border/60 bg-card px-2 text-ui-base",
        "text-muted-foreground transition-colors hover:border-border hover:bg-accent hover:text-foreground",
        "animate-in slide-in-from-top-2 duration-200 ease-out",
      )}
      title={tr("Open AI agent")}
    >
      <span>{tr("Open AI agent")}</span>
      <Kbd className="h-4 min-w-4 px-1">{fmtShortcut(MOD_KEY, "I")}</Kbd>
    </button>
  );
}

export function AiStatusBarControls({
  embedded = false,
  spread = false,
}: {
  embedded?: boolean;
  spread?: boolean;
}) {
  const tr = useTranslation();
  const c = useComposer();
  const toggleMini = useChatStore((s) => s.toggleMini);
  const miniOpen = useChatStore((s) => s.mini.open);
  const closePanel = useChatStore((s) => s.closePanel);

  return (
    <div
      className={cn(
        "flex min-w-0 flex-wrap items-center gap-x-0.5 gap-y-1.5",
        spread && "w-full",
      )}
    >
      <ComposerActionMenu />

      <ComposerPermissions />
      <AgentSwitcher />
      <div
        className={cn(
          "flex min-w-0 max-w-full items-center gap-0.5",
          spread && "ml-auto",
        )}
      >
        <ComposerModelSelector />
        <ComposerReasoningSelector />

        {!embedded && (
          <>
            <span className="mx-1 h-8 w-px bg-border" aria-hidden />
            <Button
              onClick={closePanel}
              title={tr("Close AI panel")}
              size="xs"
              variant="ghost"
              aria-label={tr("Close AI panel")}
              className="text-ui-base text-foreground/85 px-1"
            >
              <Kbd className="h-4 gap-px px-2 font-mono text-ui-xs">
                {fmtShortcut(MOD_KEY, "I")}
              </Kbd>
            </Button>
            <IconBtn
              title={tr("{value0} AI chat window ({value1})", {
                value0: miniOpen ? tr("Close") : tr("Open"),
                value1: fmtShortcut("⇧", MOD_KEY, "I"),
              })}
              onClick={toggleMini}
            >
              <HugeiconsIcon
                icon={Message01Icon}
                size={13}
                strokeWidth={1.75}
              />
            </IconBtn>
          </>
        )}
        {c.isBusy ? (
          <Button
            type="button"
            size="icon"
            variant="ghost"
            onClick={c.stop}
            className="ml-1 size-7 shrink-0 rounded-full"
            aria-label={tr("Stop")}
            title={tr("Stop")}
          >
            <HugeiconsIcon icon={StopCircleIcon} size={13} strokeWidth={1.75} />
          </Button>
        ) : (
          <Button
            type="button"
            size="icon"
            onClick={c.submit}
            disabled={!c.canSend}
            className="ml-1 size-7 shrink-0 rounded-full"
            aria-label={tr("Send")}
            title={tr("Send (Enter)")}
          >
            <HugeiconsIcon icon={ArrowUpIcon} size={13} strokeWidth={1.75} />
          </Button>
        )}
      </div>
    </div>
  );
}

function IconBtn({
  title,
  onClick,
  disabled,
  className,
  children,
}: {
  title: string;
  onClick: () => void;
  disabled?: boolean;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <Button
      type="button"
      variant="ghost"
      size="icon"
      title={title}
      onClick={onClick}
      disabled={disabled}
      className={cn(
        "size-6 rounded-md text-muted-foreground hover:text-foreground",
        className,
      )}
    >
      {children}
    </Button>
  );
}
