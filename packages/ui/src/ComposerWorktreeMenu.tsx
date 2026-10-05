import { useCallback, useEffect, useMemo, useState } from "react";
import type {
  GitRepositorySummary,
  GitWorktreeEntry,
  GitWorktreeHandoffInput,
  IPlatformService,
} from "@zcode/shared";
import type { IServiceAccessor } from "@zcode/services";
import { ChevronDownIcon, FolderGit2Icon, GitBranchIcon, LoaderIcon, PlusIcon, Trash2Icon } from "lucide-react";
import { Button } from "@/components/ui/button.js";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog.js";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu.js";
import { Input } from "@/components/ui/input.js";
import { cn } from "@/components/lib/utils.js";
import { toast } from "@/components/ui/toast.js";
import { useZCodeIntl } from "@/i18n/IntlProvider.js";

interface ComposerWorktreeMenuProps {
  workspacePath: string;
  gitSummary: GitRepositorySummary;
  platform: IPlatformService;
  services: IServiceAccessor;
  activeSessionId: string | null;
  readOnly?: boolean;
  /** 注册/激活目标 workspace；由 Root 负责 tab 与 session workspace 身份。 */
  onOpenWorkspacePath?: (path: string) => Promise<void>;
  /** handoff 完成后重新选择同一个权威 Session。 */
  onSelectTask?: (path: string, taskId: string) => void;
  /** 创建后以现有“新任务落点”路径登记新 worktree。 */
  onCreateWorkspace: (path: string) => void;
  onRefreshGit?: () => void;
}

function pathsEqual(left: string, right: string): boolean {
  const normalize = (value: string) => value.replace(/[\\/]+/g, "/").replace(/\/$/, "");
  const isWindowsAbsolute = (value: string) => {
    const slashValue = value.replace(/\\/g, "/");
    return /^[A-Za-z]:\//.test(slashValue) || slashValue.startsWith("//");
  };
  const normalizedLeft = normalize(left);
  const normalizedRight = normalize(right);
  return isWindowsAbsolute(left) || isWindowsAbsolute(right)
    ? normalizedLeft.toLowerCase() === normalizedRight.toLowerCase()
    : normalizedLeft === normalizedRight;
}

function pathLeaf(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

function worktreeLabel(entry: GitWorktreeEntry, detachedLabel: string): string {
  return entry.branch?.trim() || (entry.detached ? `(${detachedLabel})` : pathLeaf(entry.path));
}

/** 将 workspace 目录名收敛为 Git ref 可接受的 handoff 分支片段；中文或全符号目录使用稳定英文回退。 */
function handoffBranchSlug(path: string): string {
  const slug = pathLeaf(path)
    .normalize("NFKD")
    .replace(/[^A-Za-z0-9._-]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/\.{2,}/g, ".")
    .replace(/^[-.]+|[-.]+$/g, "")
    .slice(0, 64);
  return slug && slug !== "." && slug !== ".." ? slug : "worktree";
}

function operationId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `worktree-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function ComposerWorktreeMenu({
  workspacePath,
  gitSummary,
  platform,
  services,
  activeSessionId,
  readOnly = false,
  onOpenWorkspacePath,
  onSelectTask,
  onCreateWorkspace,
  onRefreshGit,
}: ComposerWorktreeMenuProps) {
  const { intl } = useZCodeIntl();
  const [open, setOpen] = useState(false);
  const [entries, setEntries] = useState<GitWorktreeEntry[]>([]);
  const [available, setAvailable] = useState(false);
  const [reason, setReason] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [pending, setPending] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [branchName, setBranchName] = useState("");
  const [createError, setCreateError] = useState<string | null>(null);

  const current = useMemo(
    () => entries.find((entry) => pathsEqual(entry.path, workspacePath)) ?? null,
    [entries, workspacePath],
  );
  const load = useCallback(async () => {
    if (!platform.listGitWorktrees) {
      setAvailable(false);
      setEntries([]);
      setReason(null);
      return;
    }
    // workspace 切换期间不能继续显示上一个仓库的 worktree；等当前路径的
    // 权威列表返回后再恢复入口，避免把旧仓库身份带到新 Session。
    setAvailable(false);
    setEntries([]);
    setReason(null);
    setLoading(true);
    try {
      const result = await platform.listGitWorktrees(workspacePath);
      setEntries(result.worktrees);
      setAvailable(result.available);
      setReason(result.reason ?? null);
    } catch (error) {
      setEntries([]);
      setAvailable(false);
      setReason(errorMessage(error));
    } finally {
      setLoading(false);
    }
  }, [platform, workspacePath]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!open) return;
    void load();
  }, [load, open]);

  const runHandoff = useCallback(
    async (target: GitWorktreeEntry) => {
      if (pathsEqual(target.path, workspacePath)) return;
      if (!onOpenWorkspacePath) {
        toast(intl.formatMessage({ id: "git.worktree.error.unsupported" }));
        return;
      }
      if (!activeSessionId) {
        await onOpenWorkspacePath(target.path);
        setOpen(false);
        return;
      }
      if (!platform.handoffGitWorkspace || !platform.stopGitWorkspaceSession) {
        toast(intl.formatMessage({ id: "git.worktree.error.unsupported" }));
        return;
      }
      setPending(true);
      try {
        const targetMode: GitWorktreeHandoffInput["targetMode"] = target.isMain
          ? "local"
          : "worktree";
        await platform.stopGitWorkspaceSession(activeSessionId);
        const result = await platform.handoffGitWorkspace({
          commandId: operationId(),
          threadId: activeSessionId,
          cwd: workspacePath,
          targetMode,
          currentBranch: gitSummary.branchName ?? undefined,
          worktreePath: workspacePath,
          ...(targetMode === "worktree"
            ? {
                associatedWorktreePath: target.path,
                associatedWorktreeBranch: target.branch ?? undefined,
                associatedWorktreeRef: target.head ?? undefined,
              }
            : {}),
        });
        await onOpenWorkspacePath(result.associatedWorktreePath);
        if (activeSessionId) {
          onSelectTask?.(result.associatedWorktreePath, activeSessionId);
        }
        onRefreshGit?.();
        setOpen(false);
      } catch (error) {
        toast(
          intl.formatMessage(
            { id: "git.worktree.error.requestFailed" },
            { error: errorMessage(error) },
          ),
        );
      } finally {
        setPending(false);
      }
    },
    [
      activeSessionId,
      gitSummary.branchName,
      intl,
      onOpenWorkspacePath,
      onRefreshGit,
      onSelectTask,
      platform,
      workspacePath,
    ],
  );

  const createWorktree = useCallback(
    async (openAfterCreate: boolean) => {
      if (!platform.createGitWorktree) {
        setCreateError(intl.formatMessage({ id: "git.worktree.error.unsupported" }));
        return;
      }
      const name = branchName.trim();
      if (!name) {
        setCreateError(intl.formatMessage({ id: "git.worktree.createDialog.nameRequired" }));
        return;
      }
      setPending(true);
      setCreateError(null);
      try {
        const result = await platform.createGitWorktree({
          cwd: workspacePath,
          ref: gitSummary.branchName || "HEAD",
          newBranch: name,
          checkoutBranch: false,
        });
        await load();
        setCreateOpen(false);
        setBranchName("");
        toast(intl.formatMessage({ id: "git.worktree.toast.created" }));
        if (openAfterCreate) {
          onCreateWorkspace(result.worktree.path);
        }
      } catch (error) {
        setCreateError(errorMessage(error));
      } finally {
        setPending(false);
      }
    },
    [branchName, gitSummary.branchName, intl, load, onCreateWorkspace, platform, workspacePath],
  );

  const handoffToNewWorktree = useCallback(async () => {
    if (
      !activeSessionId ||
      !platform.handoffGitWorkspace ||
      !platform.stopGitWorkspaceSession ||
      !onOpenWorkspacePath
    ) {
      toast(intl.formatMessage({ id: "git.worktree.error.unsupported" }));
      return;
    }
    setPending(true);
    try {
      await platform.stopGitWorkspaceSession(activeSessionId);
      const result = await platform.handoffGitWorkspace({
        commandId: operationId(),
        threadId: activeSessionId,
        cwd: workspacePath,
        targetMode: "worktree",
        currentBranch: gitSummary.branchName ?? undefined,
        worktreePath: workspacePath,
        preferredWorktreeBaseBranch: gitSummary.branchName || "HEAD",
        preferredNewWorktreeName: `${handoffBranchSlug(workspacePath)}-handoff-${Date.now().toString(36)}`,
      });
      await onOpenWorkspacePath(result.associatedWorktreePath);
      onSelectTask?.(result.associatedWorktreePath, activeSessionId);
      onRefreshGit?.();
      setOpen(false);
    } catch (error) {
      toast(
        intl.formatMessage(
          { id: "git.worktree.error.requestFailed" },
          { error: errorMessage(error) },
        ),
      );
    } finally {
      setPending(false);
    }
  }, [
    activeSessionId,
    gitSummary.branchName,
    intl,
    onOpenWorkspacePath,
    onRefreshGit,
    onSelectTask,
    platform,
    workspacePath,
  ]);

  const removeWorktree = useCallback(
    async (target: GitWorktreeEntry) => {
      if (!platform.removeGitWorktree || target.isMain || target.prunable) return;
      setPending(true);
      try {
        await platform.removeGitWorktree({
          cwd: workspacePath,
          path: target.path,
          force: false,
          reclaimTemporaryBranch: false,
        });
        await load();
        onRefreshGit?.();
      } catch (error) {
        toast(
          intl.formatMessage(
            { id: "git.worktree.error.requestFailed" },
            { error: errorMessage(error) },
          ),
        );
      } finally {
        setPending(false);
      }
    },
    [intl, load, onRefreshGit, platform, workspacePath],
  );

  const archiveCurrentWorktree = useCallback(async () => {
    if (!activeSessionId || !current || current.isMain || !platform.archiveGitWorktree) {
      toast(intl.formatMessage({ id: "git.worktree.error.archiveUnavailable" }));
      return;
    }
    if (!platform.stopGitWorkspaceSession) {
      toast(intl.formatMessage({ id: "git.worktree.error.unsupported" }));
      return;
    }
    setPending(true);
    try {
      // 先关闭执行资源，再写入归档 Journal；否则 cleanup 会拒绝活动 reservation，留下半归档状态。
      await platform.stopGitWorkspaceSession(activeSessionId);
      const receipt = await services.zcodeTaskService.archiveTaskWithReceipt({
        taskId: activeSessionId,
        workspacePath,
      });
      await platform.archiveGitWorktree({
        cwd: workspacePath,
        path: current.path,
        threadId: activeSessionId,
        operationId: receipt.operationId,
        journalSequence: receipt.journalSequence,
      });
      await load();
      toast(intl.formatMessage({ id: "git.worktree.toast.archived" }));
      const main = entries.find((entry) => entry.isMain);
      if (main && onOpenWorkspacePath) {
        await onOpenWorkspacePath(main.path);
      }
    } catch (error) {
      toast(
        intl.formatMessage(
          { id: "git.worktree.error.requestFailed" },
          { error: errorMessage(error) },
        ),
      );
    } finally {
      setPending(false);
    }
  }, [activeSessionId, current, entries, intl, load, onOpenWorkspacePath, platform, services, workspacePath]);

  // 切换到新建 linked worktree 时，Git 面板摘要会先重置为初始空态；此时
  // listGitWorktrees 已是同一 workspace 的权威查询，不能因摘要尚未刷新而丢失入口。
  if (
    !platform.listGitWorktrees ||
    ((!gitSummary.isGitAvailable || !gitSummary.isRepository) && !available)
  ) {
    return null;
  }

  const detachedLabel = intl.formatMessage({ id: "git.worktree.detached" });
  // 分支选择器已展示当前分支，工作树入口使用独立名称，避免并排出现两个 main。
  // 当前工作树身份继续来自 Rust 列表；tooltip 保留目录及分支，数据属性供原生验收核对。
  const currentDescription = current
    ? `${pathLeaf(current.path)} · ${worktreeLabel(current, detachedLabel)}`
    : undefined;
  const canMutate = !readOnly && !pending;

  return (
    <>
      <DropdownMenu open={open} onOpenChange={setOpen}>
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            size="default"
            disabled={pending}
            data-testid="workspace-worktree-open"
            data-worktree-path={current?.path}
            data-worktree-branch={current?.branch ?? undefined}
            aria-label={intl.formatMessage({ id: "git.worktree.trigger.ariaLabel" })}
            title={currentDescription}
            className="min-w-0 max-w-full rounded-full px-2 text-ui-base/relaxed"
          >
            <FolderGit2Icon className="size-4 text-foreground-subtle" />
            <span className="min-w-0 max-w-32 truncate">
              {intl.formatMessage({ id: "git.worktree.section" })}
            </span>
            {loading ? (
              <LoaderIcon className="size-3.5 animate-spin text-foreground-subtle" />
            ) : (
              <ChevronDownIcon className="size-3.5 text-foreground-subtle" />
            )}
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent
          align="start"
          side="top"
          className="w-80"
          data-testid="workspace-worktree-menu"
        >
          <DropdownMenuLabel>{intl.formatMessage({ id: "git.worktree.section" })}</DropdownMenuLabel>
          {!available ? (
            <div className="px-2 py-2 text-ui-sm text-foreground-subtle">
              {reason || intl.formatMessage({ id: "git.worktree.unavailable" })}
            </div>
          ) : entries.length === 0 ? (
            <div className="px-2 py-2 text-ui-sm text-foreground-subtle">
              {loading
                ? intl.formatMessage({ id: "git.worktree.loading" })
                : intl.formatMessage({ id: "git.worktree.empty" })}
            </div>
          ) : (
            <DropdownMenuRadioGroup
              value={current?.path ?? workspacePath}
              onValueChange={(path) => {
                const target = entries.find((entry) => pathsEqual(entry.path, path));
                if (target) void runHandoff(target);
              }}
            >
              {entries.map((entry) => {
                const isCurrent = pathsEqual(entry.path, workspacePath);
                const meta = [
                  entry.isMain ? intl.formatMessage({ id: "git.worktree.main" }) : null,
                  entry.detached ? intl.formatMessage({ id: "git.worktree.detached" }) : null,
                  entry.prunable ? intl.formatMessage({ id: "git.worktree.prunable" }) : null,
                  isCurrent ? intl.formatMessage({ id: "git.worktree.current" }) : null,
                ]
                  .filter(Boolean)
                  .join(" · ");
                return (
                  <div key={entry.path} className="group/worktree-row flex items-start gap-1">
                    <DropdownMenuRadioItem
                      value={entry.path}
                      // 活动 Session 的已有目标必须带匹配的 association proof；菜单没有该事实时，
                      // 只保留下方由 Rust 创建并绑定的新 handoff worktree，避免展示必失败动作。
                      disabled={!canMutate || isCurrent || Boolean(activeSessionId)}
                      data-testid={isCurrent ? "workspace-worktree-current" : "workspace-worktree-row"}
                      className={cn("min-w-0 flex-1 items-start gap-2 py-2")}
                      title={entry.path}
                    >
                      <GitBranchIcon className="mt-0.5 size-4 text-foreground-subtle" />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-ui-base font-medium">
                          {worktreeLabel(entry, detachedLabel)}
                        </span>
                        <span className="block truncate text-ui-xs text-foreground-subtle">
                          {meta || entry.path}
                        </span>
                      </span>
                    </DropdownMenuRadioItem>
                    {!entry.isMain && !isCurrent && !entry.prunable ? (
                      <DropdownMenuItem
                        disabled={!canMutate}
                        data-testid="workspace-worktree-remove"
                        data-worktree-branch={entry.branch ?? undefined}
                        aria-label={intl.formatMessage({ id: "git.worktree.remove" })}
                        className="mt-1 size-7 justify-center p-0"
                        onSelect={(event) => {
                          event.preventDefault();
                          void removeWorktree(entry);
                        }}
                      >
                        <Trash2Icon className="size-3.5" />
                      </DropdownMenuItem>
                    ) : null}
                  </div>
                );
              })}
            </DropdownMenuRadioGroup>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem
            disabled={
              !canMutate ||
              !activeSessionId ||
              !platform.handoffGitWorkspace ||
              !platform.stopGitWorkspaceSession
            }
            data-testid="workspace-session-handoff"
            onSelect={() => {
              void handoffToNewWorktree();
            }}
          >
            <GitBranchIcon />
            {intl.formatMessage({ id: "git.worktree.handoff" })}
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={!canMutate || !platform.createGitWorktree}
            data-testid="workspace-worktree-create"
            onSelect={() => {
              setCreateError(null);
              setCreateOpen(true);
            }}
          >
            <PlusIcon />
            {intl.formatMessage({ id: "git.worktree.create" })}
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={!canMutate || !activeSessionId || !current || current.isMain || !platform.archiveGitWorktree}
            data-testid="workspace-worktree-archive"
            onSelect={() => {
              void archiveCurrentWorktree();
            }}
          >
            <Trash2Icon />
            {intl.formatMessage({ id: "git.worktree.archive" })}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>

      <Dialog open={createOpen} onOpenChange={setCreateOpen}>
        <DialogContent className="max-w-md" data-testid="workspace-worktree-create-dialog">
          <DialogHeader>
            <DialogTitle>{intl.formatMessage({ id: "git.worktree.createDialog.title" })}</DialogTitle>
            <DialogDescription>
              {intl.formatMessage({ id: "git.worktree.createDialog.description" })}
            </DialogDescription>
          </DialogHeader>
          <label className="grid gap-1.5 text-ui-sm" htmlFor="workspace-worktree-branch-name">
            {intl.formatMessage({ id: "git.worktree.createDialog.nameLabel" })}
            <Input
              id="workspace-worktree-branch-name"
              data-testid="workspace-worktree-create-name"
              value={branchName}
              onChange={(event) => setBranchName(event.target.value)}
              placeholder={intl.formatMessage({ id: "git.worktree.createDialog.placeholder" })}
              disabled={pending}
              autoFocus
            />
          </label>
          {createError ? <p className="text-ui-sm text-destructive">{createError}</p> : null}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setCreateOpen(false)} disabled={pending}>
              {intl.formatMessage({ id: "common.cancel" })}
            </Button>
            <Button
              type="button"
              data-testid="workspace-worktree-create-submit"
              onClick={() => void createWorktree(true)}
              disabled={pending || !branchName.trim()}
            >
              {pending ? <LoaderIcon className="animate-spin" /> : <PlusIcon />}
              {intl.formatMessage({ id: "git.worktree.createDialog.confirm" })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
