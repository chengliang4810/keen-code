import { useTranslation } from "@/modules/i18n";
import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import {
  ArrowDown01Icon,
  ArrowUp01Icon,
  CheckmarkCircle02Icon,
  Loading03Icon,
  Notification01Icon,
  Notification03Icon,
} from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { invoke } from "@tauri-apps/api/core";
import { useMemo, useState } from "react";
import { AgentIcon } from "../lib/agentIcon";
import { displayAgent } from "../lib/format";
import type { AgentNotification, AgentStatus } from "../lib/types";
import { useAgentStore } from "../store/agentStore";

type Props = {
  onActivate: (tabId: number, leafId: number) => void;
  onActivateLocal: () => void;
};

function relativeTime(
  ts: number,
  tr: ReturnType<typeof useTranslation>,
): string {
  const s = Math.floor((Date.now() - ts) / 1000);
  if (s < 60) return tr("just now");
  const m = Math.floor(s / 60);
  if (m < 60) return tr("{count}m ago", { count: m });
  const h = Math.floor(m / 60);
  if (h < 24) return tr("{count}h ago", { count: h });
  return tr("{count}d ago", { count: Math.floor(h / 24) });
}

function StatusRow({
  agent,
  status,
  onClick,
}: {
  agent: string;
  status: AgentStatus;
  onClick: () => void;
}) {
  const tr = useTranslation();
  const waiting = status === "waiting";
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex w-full items-center gap-2.5 rounded-lg px-2 py-2 text-left transition-colors hover:bg-accent"
    >
      <AgentIcon
        agent={agent}
        size={16}
        className="shrink-0 text-muted-foreground"
      />
      <span className="flex-1 truncate text-ui-base text-foreground">
        {displayAgent(agent)}
      </span>
      <span
        className={cn(
          "flex items-center gap-1.5 text-ui-sm",
          waiting ? "font-medium text-primary" : "text-muted-foreground",
        )}
      >
        {waiting ? <span className="size-1.5 rounded-full bg-primary" /> : null}
        {waiting ? tr("waiting") : tr("working")}
      </span>
    </button>
  );
}

const NOTIF_LABEL: Record<AgentNotification["kind"], string> = {
  attention: "needs input",
  finished: "finished",
  error: "failed",
};

const HOOK_AGENTS = ["claude", "codex", "gemini", "pi"] as const;

function HookAgentRow({
  id,
  label,
  ready,
  installing,
  onEnable,
}: {
  id: string;
  label: string;
  ready: boolean;
  installing: boolean;
  onEnable: () => void;
}) {
  const tr = useTranslation();
  return (
    <div className="flex items-center gap-2 px-2 py-1">
      <AgentIcon
        agent={id}
        size={14}
        className="shrink-0 text-muted-foreground"
      />
      <span className="flex-1 truncate text-ui-base text-muted-foreground">
        {label}
      </span>
      {ready ? (
        <span className="flex items-center gap-1 text-ui-sm font-medium text-primary">
          <HugeiconsIcon
            icon={CheckmarkCircle02Icon}
            size={13}
            strokeWidth={1.75}
          />
          {tr("enabled")}
        </span>
      ) : (
        <button
          type="button"
          onClick={onEnable}
          disabled={installing}
          className="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-ui-base text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-60"
        >
          {installing ? (
            <HugeiconsIcon
              icon={Loading03Icon}
              size={12}
              strokeWidth={1.75}
              className="animate-spin"
            />
          ) : null}
          {installing ? tr("Enabling") : tr("Enable")}
        </button>
      )}
    </div>
  );
}

function NotificationRow({
  n,
  onClick,
}: {
  n: AgentNotification;
  onClick: () => void;
}) {
  const tr = useTranslation();
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex w-full items-center gap-2.5 rounded-lg px-2 py-2 text-left transition-colors hover:bg-accent"
    >
      <span className="flex w-4 shrink-0 items-center justify-center">
        {n.kind === "finished" ? (
          <HugeiconsIcon
            icon={CheckmarkCircle02Icon}
            size={15}
            strokeWidth={1.75}
            className="text-muted-foreground"
          />
        ) : (
          <span
            className={cn(
              "size-1.5 rounded-full",
              n.kind === "error" ? "bg-destructive" : "bg-primary",
            )}
          />
        )}
      </span>
      <span className="min-w-0 flex-1 truncate text-ui-base text-foreground">
        {displayAgent(n.agent)}{" "}
        <span className="text-muted-foreground">{tr(NOTIF_LABEL[n.kind])}</span>
      </span>
      <span className="shrink-0 text-ui-xs tabular-nums text-muted-foreground">
        {relativeTime(n.at, tr)}
      </span>
    </button>
  );
}

export function NotificationBell({ onActivate, onActivateLocal }: Props) {
  const tr = useTranslation();
  const [open, setOpen] = useState(false);
  const [hooks, setHooks] = useState<Record<string, boolean>>({});
  const [installing, setInstalling] = useState<string | null>(null);
  const [alertsOpen, setAlertsOpen] = useState(false);
  const sessions = useAgentStore((s) => s.sessions);
  const localAgent = useAgentStore((s) => s.localAgent);
  const notifications = useAgentStore((s) => s.notifications);
  const markAllRead = useAgentStore((s) => s.markAllRead);
  const clearNotifications = useAgentStore((s) => s.clearNotifications);

  const active = useMemo(() => Object.values(sessions), [sessions]);
  const activeCount = active.length + (localAgent ? 1 : 0);
  const waitingCount =
    active.filter((s) => s.status === "waiting").length +
    (localAgent?.status === "waiting" ? 1 : 0);
  // attention maps to an active waiting session, so only completed events add
  // to the badge to avoid double-counting.
  const unreadDone = notifications.filter(
    (n) => !n.read && n.kind !== "attention",
  ).length;
  const badge = waitingCount + unreadDone;
  const enabledCount = HOOK_AGENTS.filter((id) => hooks[id] === true).length;

  const refreshHooks = () => {
    for (const id of HOOK_AGENTS) {
      invoke<boolean>("agent_hooks_status", { agent: id })
        .then((ok) => setHooks((h) => ({ ...h, [id]: ok })))
        .catch(() => setHooks((h) => ({ ...h, [id]: false })));
    }
  };

  const onOpenChange = (next: boolean) => {
    setOpen(next);
    if (next) {
      markAllRead();
      refreshHooks();
    }
  };

  const enableHooks = async (id: string) => {
    setInstalling(id);
    try {
      await invoke("agent_enable_hooks", { agent: id });
      setHooks((h) => ({ ...h, [id]: true }));
    } catch {
      setHooks((h) => ({ ...h, [id]: false }));
    } finally {
      setInstalling(null);
    }
  };

  const activate = (tabId: number, leafId: number) => {
    onActivate(tabId, leafId);
    setOpen(false);
  };

  const activateLocal = () => {
    onActivateLocal();
    setOpen(false);
  };

  const activateNotification = (n: AgentNotification) => {
    if (n.source === "local") activateLocal();
    else activate(n.tabId, n.leafId);
  };

  const empty = activeCount === 0 && notifications.length === 0;

  return (
    <Popover open={open} onOpenChange={onOpenChange}>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="relative size-7 shrink-0 rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
          title={tr("Agent notifications")}
        >
          <HugeiconsIcon
            icon={Notification01Icon}
            size={16}
            strokeWidth={1.75}
          />
          {badge > 0 ? (
            <span className="absolute -top-0.5 -right-0.5 flex h-3.5 min-w-3.5 items-center justify-center rounded-full bg-primary px-0.5 text-ui-xs font-semibold leading-none text-primary-foreground">
              {badge > 9 ? "9+" : badge}
            </span>
          ) : null}
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        sideOffset={8}
        className="w-80 overflow-hidden p-0 gap-0.5"
      >
        <div className="flex h-10 items-center gap-2 px-3 pt-0.5">
          <span className="flex gap-1 text-ui-base text-foreground">
            {tr("Notifications")}
          </span>
          <div className="ml-auto flex items-center gap-2">
            {activeCount > 0 ? (
              <span className="rounded-full bg-accent px-1.5 py-0.5 text-ui-xs font-medium tabular-nums text-muted-foreground">
                {activeCount} {tr("active")}
              </span>
            ) : null}
            {notifications.length > 0 ? (
              <button
                type="button"
                onClick={clearNotifications}
                className="rounded-md px-1.5 py-0.5 text-ui-base text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
              >
                {tr("Clear")}
              </button>
            ) : null}
          </div>
        </div>

        {empty ? (
          <div className="border-t border-border/60 px-3 py-5 text-center text-ui-sm leading-relaxed text-muted-foreground">
            {tr("No agent activity yet.")}
            <br />
            {tr("Run the RCode agent or a coding agent to track it here.")}
          </div>
        ) : (
          <div className="max-h-80 overflow-y-auto border-t border-border/60 p-1">
            {localAgent ? (
              <StatusRow
                agent={localAgent.agent}
                status={localAgent.status}
                onClick={activateLocal}
              />
            ) : null}
            {active.map((s) => (
              <StatusRow
                key={s.leafId}
                agent={s.agent}
                status={s.status}
                onClick={() => activate(s.tabId, s.leafId)}
              />
            ))}
            {activeCount > 0 && notifications.length > 0 ? (
              <div className="mx-2 my-1 h-px bg-border/50" />
            ) : null}
            {notifications.map((n) => (
              <NotificationRow
                key={n.id}
                n={n}
                onClick={() => activateNotification(n)}
              />
            ))}
          </div>
        )}

        <div className="border-t border-border/60 p-1">
          <button
            type="button"
            onClick={() => setAlertsOpen((v) => !v)}
            aria-expanded={alertsOpen}
            className="flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-ui-base font-medium uppercase tracking-wide text-muted-foreground/70 transition-colors hover:text-foreground"
          >
            <HugeiconsIcon
              icon={Notification03Icon}
              size={11}
              strokeWidth={2}
            />
            {tr("Agent alerts")}
            <span className="ml-auto flex items-center gap-1.5 normal-case tracking-normal">
              {enabledCount > 0 ? (
                <span className="text-ui-xs text-muted-foreground/60">
                  {enabledCount} {tr("on")}
                </span>
              ) : null}
              <HugeiconsIcon
                icon={alertsOpen ? ArrowUp01Icon : ArrowDown01Icon}
                size={13}
                strokeWidth={2}
              />
            </span>
          </button>
          {alertsOpen
            ? HOOK_AGENTS.map((id) => (
                <HookAgentRow
                  key={id}
                  id={id}
                  label={displayAgent(id)}
                  ready={hooks[id] === true}
                  installing={installing === id}
                  onEnable={() => enableHooks(id)}
                />
              ))
            : null}
        </div>
      </PopoverContent>
    </Popover>
  );
}
