import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  Delete02Icon,
  Folder01Icon,
  MoreHorizontalIcon,
  Search01Icon,
} from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@/components/ui/input-group";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/localized/dialog";
import { Spinner } from "@/components/localized/spinner";
import { useLocale, useTranslation } from "@/modules/i18n";
import {
  groupArchivedConversations,
  type ArchiveKind,
  type ArchiveOperation,
  type ArchivedGroup,
} from "@/modules/ai/lib/archivedConversations";
import {
  requestArchives,
  watchArchives,
  type ArchiveSnapshot,
} from "@/modules/ai/lib/archiveManagement";
import { SectionHeader } from "@/settings/components/SectionHeader";

type Deletion = { ids: string[]; label: string };

export function ArchivedConversationsSection() {
  const tr = useTranslation();
  const locale = useLocale();
  const [data, setData] = useState<ArchiveSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [kind, setKind] = useState<ArchiveKind>("all");
  const [project, setProject] = useState("all");
  const [busy, setBusy] = useState(false);
  const [deletion, setDeletion] = useState<Deletion | null>(null);
  const revision = useRef(0);
  const mounted = useRef(true);
  const working = useRef(false);
  const refresh = useCallback(async () => {
    const request = ++revision.current;
    try {
      const snapshot = await requestArchives();
      if (mounted.current && request === revision.current) {
        setData(snapshot);
        setError(null);
      }
    } catch (cause) {
      if (mounted.current && request === revision.current)
        setError(cause instanceof Error ? cause.message : String(cause));
    }
  }, []);
  useEffect(() => {
    mounted.current = true;
    void refresh();
    const stop = watchArchives(() => {
      if (!working.current) void refresh();
    });
    const focus = () => {
      if (!working.current) void refresh();
    };
    window.addEventListener("focus", focus);
    return () => {
      mounted.current = false;
      ++revision.current;
      window.removeEventListener("focus", focus);
      void stop.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [refresh]);

  const perform = async (operation: ArchiveOperation, ids: string[]) => {
    if (working.current) return;
    working.current = true;
    ++revision.current;
    setBusy(true);
    setError(null);
    try {
      const snapshot = await requestArchives(operation, ids);
      if (mounted.current) {
        ++revision.current;
        setData(snapshot);
        setDeletion(null);
      }
    } catch (cause) {
      if (mounted.current)
        setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      working.current = false;
      if (mounted.current) setBusy(false);
    }
  };
  const allGroups = useMemo(
    () =>
      groupArchivedConversations(data?.sessions ?? [], data?.projects ?? []),
    [data],
  );
  const groups = useMemo(
    () =>
      groupArchivedConversations(
        data?.sessions ?? [],
        data?.projects ?? [],
        search,
        kind,
        project,
      ),
    [data, search, kind, project],
  );
  const groupName = (group: ArchivedGroup) =>
    group.id === "independent"
      ? tr("No project")
      : (group.project?.name ?? tr("Unavailable project"));
  const disabled = busy || !data || data.locked;
  const formatter = useMemo(
    () =>
      new Intl.DateTimeFormat(locale, {
        year: "numeric",
        month: "long",
        day: "numeric",
        hour: "numeric",
        minute: "2-digit",
      }),
    [locale],
  );

  return (
    <div className="flex flex-col gap-8">
      <div className="flex items-center justify-between gap-4">
        <SectionHeader title={tr("Archived conversations")} />
        <Button
          variant="ghost"
          size="sm"
          className="shrink-0 gap-1.5 rounded-md bg-destructive/10 text-ui-base text-destructive hover:bg-destructive/15 hover:text-destructive"
          disabled={disabled || !data.sessions.length}
          onClick={() =>
            data &&
            setDeletion({
              ids: data.sessions.map((session) => session.id),
              label: tr("All archived conversations"),
            })
          }
        >
          <HugeiconsIcon icon={Delete02Icon} size={14} />
          {tr("Delete all")}
        </Button>
      </div>
      <div className="grid grid-cols-[minmax(0,1fr)_144px_176px] items-center gap-2 max-[1000px]:grid-cols-2">
        <InputGroup className="h-8 rounded-md max-[1000px]:col-span-2">
          <InputGroupAddon>
            <HugeiconsIcon icon={Search01Icon} size={14} />
          </InputGroupAddon>
          <InputGroupInput
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder={tr("Search archived conversations")}
            aria-label={tr("Search archived conversations")}
            className="text-ui-base"
            disabled={busy}
          />
        </InputGroup>
        <Select
          value={kind}
          onValueChange={(value) => {
            setKind(value as ArchiveKind);
            setProject("all");
          }}
          disabled={busy}
        >
          <SelectTrigger
            className="h-8! w-full rounded-md text-ui-base"
            aria-label={tr("Conversation type")}
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{tr("All conversations")}</SelectItem>
            <SelectItem value="project">
              {tr("Project conversations")}
            </SelectItem>
            <SelectItem value="independent">
              {tr("Independent conversations")}
            </SelectItem>
          </SelectContent>
        </Select>
        <Select
          value={project}
          onValueChange={setProject}
          disabled={busy || kind === "independent"}
        >
          <SelectTrigger
            className="h-8! w-full rounded-md text-ui-base"
            aria-label={tr("Filter by project")}
          >
            <HugeiconsIcon icon={Folder01Icon} size={14} />
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{tr("All projects")}</SelectItem>
            {allGroups
              .filter((group) => group.id !== "independent")
              .map((group) => (
                <SelectItem key={group.id} value={group.id}>
                  {groupName(group)}
                </SelectItem>
              ))}
          </SelectContent>
        </Select>
      </div>
      {error && (
        <div
          role="alert"
          className="flex items-center justify-between gap-3 text-ui-base text-destructive"
        >
          <p>{tr(error)}</p>
          <Button
            variant="ghost"
            size="sm"
            disabled={busy}
            onClick={() => void refresh()}
          >
            {tr("Retry")}
          </Button>
        </div>
      )}
      {data?.locked && (
        <p className="text-ui-sm text-muted-foreground">
          {tr(
            "Finish the current agent operation before managing archived conversations.",
          )}
        </p>
      )}
      {!data && !error && (
        <div className="flex items-center gap-2 text-ui-sm text-muted-foreground">
          <Spinner className="size-3" />
          {tr("Loading archived conversations…")}
        </div>
      )}
      {data && groups.length === 0 && (
        <p className="py-10 text-center text-ui-sm text-muted-foreground">
          {tr(
            data.sessions.length
              ? "No matching archived conversations"
              : "No archived conversations",
          )}
        </p>
      )}
      <div className="flex flex-col gap-10">
        {groups.map((group) => (
          <section
            key={group.id}
            aria-label={groupName(group)}
            className="flex flex-col gap-3"
          >
            <div className="flex items-center gap-2 text-ui-base">
              <HugeiconsIcon
                icon={Folder01Icon}
                size={14}
                className="shrink-0 text-muted-foreground"
                aria-hidden="true"
              />
              <span
                className="min-w-0 flex-1 truncate font-medium"
                title={group.project?.root ?? undefined}
              >
                {groupName(group)}
              </span>
              <span className="shrink-0 text-muted-foreground">
                {tr("{count} conversations", { count: group.sessions.length })}
              </span>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    size="icon-xs"
                    variant="ghost"
                    className="size-6 rounded-md"
                    disabled={disabled}
                    aria-label={tr("Archived project actions")}
                  >
                    <HugeiconsIcon icon={MoreHorizontalIcon} size={14} />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  <DropdownMenuItem
                    variant="destructive"
                    onSelect={() =>
                      setDeletion({
                        ids: (
                          allGroups.find((item) => item.id === group.id)
                            ?.sessions ?? []
                        ).map((session) => session.id),
                        label: groupName(group),
                      })
                    }
                  >
                    <HugeiconsIcon icon={Delete02Icon} size={14} />
                    {tr("Delete archived conversations in this group")}
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
            {group.project?.removed && (
              <p className="text-ui-sm text-muted-foreground">
                {tr(
                  "This project was removed. Add its directory again to view restored conversations in the sidebar.",
                )}
              </p>
            )}
            <div className="rounded-lg border border-border/60 px-4">
              {group.sessions.map((session) => (
                <div
                  key={session.id}
                  className="flex min-h-16 items-center gap-3 border-b border-border/60 py-3 last:border-b-0"
                >
                  <div className="min-w-0 flex-1">
                    <p
                      className="truncate text-ui-base font-medium"
                      title={session.title}
                    >
                      {session.title === "New chat"
                        ? tr("New task")
                        : session.title}
                    </p>
                    <time
                      className="mt-1 block text-ui-caption text-muted-foreground"
                      dateTime={new Date(session.updatedAt).toISOString()}
                    >
                      {formatter.format(session.updatedAt)}
                    </time>
                  </div>
                  <Button
                    size="icon-xs"
                    variant="ghost"
                    className="size-7 shrink-0 rounded-md text-muted-foreground hover:text-destructive"
                    disabled={disabled}
                    aria-label={tr("Delete archived conversation: {title}", {
                      title: session.title,
                    })}
                    onClick={() =>
                      setDeletion({ ids: [session.id], label: session.title })
                    }
                  >
                    <HugeiconsIcon icon={Delete02Icon} size={14} />
                  </Button>
                  <Button
                    variant="secondary"
                    size="sm"
                    className="h-7 shrink-0 rounded-md px-2 text-ui-base font-normal"
                    disabled={disabled}
                    onClick={() => void perform("restore", [session.id])}
                  >
                    {tr("Unarchive")}
                  </Button>
                </div>
              ))}
            </div>
          </section>
        ))}
      </div>
      <Dialog
        open={deletion !== null}
        onOpenChange={(open) => {
          if (!open && !busy) setDeletion(null);
        }}
      >
        <DialogContent showCloseButton={!busy}>
          <DialogHeader>
            <DialogTitle>{tr("Delete archived conversations?")}</DialogTitle>
            <DialogDescription>
              {tr(
                "Permanently delete {count} archived conversations from {name}? Their messages cannot be recovered.",
                {
                  count: deletion?.ids.length ?? 0,
                  name: deletion?.label ?? "",
                },
              )}
            </DialogDescription>
          </DialogHeader>
          {error && (
            <p role="alert" className="text-ui-sm text-destructive">
              {tr(error)}
            </p>
          )}
          <DialogFooter>
            <Button
              variant="ghost"
              disabled={busy}
              onClick={() => setDeletion(null)}
            >
              {tr("Cancel")}
            </Button>
            <Button
              variant="destructive"
              disabled={busy || !deletion?.ids.length}
              onClick={() => deletion && void perform("delete", deletion.ids)}
            >
              {busy && <Spinner className="size-3" />}
              {tr("Delete")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
