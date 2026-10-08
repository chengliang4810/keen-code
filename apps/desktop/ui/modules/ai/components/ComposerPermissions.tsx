import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { useComposer } from "@/modules/ai/lib/composer";
import {
  normalizePermissionMode,
  type PermissionMode,
} from "@/modules/ai/lib/permissions";
import { useChatStore } from "@/modules/ai/store/chatStore";
import { usePlanStore } from "@/modules/ai/store/planStore";
import { useTranslation } from "@/modules/i18n";
import {
  ArrowDown01Icon,
  BrainIcon,
  Settings01Icon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";

const MODES: { id: PermissionMode; label: string; description: string }[] = [
  {
    id: "edit",
    label: "Auto approval",
    description: "Edit workspace files automatically; confirm commands.",
  },
  {
    id: "full-access",
    label: "Full access",
    description: "Edit files and run commands without individual confirmation.",
  },
];

export function ComposerPermissions() {
  const tr = useTranslation();
  const c = useComposer();
  const sessionId = useChatStore((s) => s.activeSessionId);
  const permissionMode = useChatStore((s) =>
    normalizePermissionMode(
      (s.draftSession?.id === s.activeSessionId
        ? s.draftSession
        : s.sessions.find((session) => session.id === s.activeSessionId)
      )?.permissionMode,
    ),
  );
  const setPermissionMode = useChatStore((s) => s.setSessionPermissionMode);
  const planMode = usePlanStore((s) => s.active);
  const selected = MODES.find((mode) => mode.id === permissionMode) ?? MODES[0];

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={c.isBusy || !sessionId}
          aria-label={tr("Agent permissions")}
          title={tr("Agent permissions")}
          className={cn(
            "h-6 shrink-0 gap-1 rounded-md px-1.5 text-ui-base text-muted-foreground",
            permissionMode === "full-access" &&
              !planMode &&
              "text-amber-600 dark:text-amber-400",
          )}
        >
          <HugeiconsIcon
            icon={planMode ? BrainIcon : Settings01Icon}
            size={13}
            strokeWidth={1.75}
          />
          <span>{tr(planMode ? "Plan mode" : selected.label)}</span>
          <HugeiconsIcon icon={ArrowDown01Icon} size={11} strokeWidth={1.75} />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="top" className="w-72">
        <DropdownMenuLabel>{tr("Agent permissions")}</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={permissionMode}
          onValueChange={(value) => {
            if (sessionId)
              setPermissionMode(sessionId, normalizePermissionMode(value));
          }}
        >
          {MODES.map((mode) => (
            <DropdownMenuRadioItem key={mode.id} value={mode.id}>
              <div className="flex flex-col gap-0.5">
                <span>{tr(mode.label)}</span>
                <span className="text-ui-sm font-normal text-muted-foreground">
                  {tr(mode.description)}
                </span>
              </div>
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
