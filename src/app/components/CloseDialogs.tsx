import { useTranslation } from "@/modules/i18n";
import type { CloseManyPending } from "@/app/hooks/tabCloseGuards";
import {
  type AppCloseBlocker,
  canOptOutOfAppClosePrompt,
} from "@/app/hooks/useAppCloseGuard";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { setConfirmCloseRunningTerminal } from "@/modules/settings/store";
import type { Tab } from "@/modules/tabs";
import { useId, useState } from "react";

type Props = {
  tabs: Tab[];
  pendingCloseTab: number | null;
  onCancelClose: () => void;
  onConfirmClose: () => void;
  pendingTerminalCloseTab: number | null;
  onCancelTerminalClose: () => void;
  onConfirmTerminalClose: () => void;
  pendingDeleteTabs: number[] | null;
  onCancelDeleteClose: () => void;
  onConfirmDeleteClose: () => void;
  pendingCloseMany: CloseManyPending | null;
  closeManyConfirming: boolean;
  onCancelCloseMany: () => void;
  onConfirmCloseMany: () => void;
  pendingAppClose: AppCloseBlocker | null;
  onCancelAppClose: () => void;
  onConfirmAppClose: () => void;
};

function appCloseMessage(
  blocker: AppCloseBlocker,
  tr: ReturnType<typeof useTranslation>,
): string {
  if (blocker.dirtyEditors > 0 && blocker.busyTerminal) {
    return tr(
      blocker.dirtyEditors === 1
        ? "A process is still running and 1 file has unsaved changes. Quitting will terminate it and discard the changes."
        : "A process is still running and {count} files have unsaved changes. Quitting will terminate it and discard the changes.",
      { count: blocker.dirtyEditors },
    );
  }
  if (blocker.dirtyEditors > 0) {
    return tr(
      blocker.dirtyEditors === 1
        ? "1 file has unsaved changes. Quitting will discard them."
        : "{count} files have unsaved changes. Quitting will discard them.",
      { count: blocker.dirtyEditors },
    );
  }
  return tr(
    "A process is still running in a terminal. Quitting will terminate it.",
  );
}

function OptOutRow({
  checked,
  onCheckedChange,
}: {
  checked: boolean;
  onCheckedChange: (value: boolean) => void;
}) {
  const tr = useTranslation();
  const id = useId();
  return (
    <div className="-mt-3 flex items-center justify-center gap-2 sm:justify-start">
      <Checkbox
        id={id}
        checked={checked}
        onCheckedChange={(value) => onCheckedChange(value === true)}
      />
      <Label
        htmlFor={id}
        className="font-normal text-ui-sm text-muted-foreground"
      >
        {tr("Don't ask again about running processes")}
      </Label>
    </div>
  );
}

async function persistOptOut(): Promise<void> {
  try {
    await setConfirmCloseRunningTerminal(false);
  } catch (e) {
    console.error("close-confirmation opt-out failed", e);
  }
}

function closeManyMessage(
  pending: CloseManyPending,
  tabs: Tab[],
  tr: ReturnType<typeof useTranslation>,
): string {
  const { kind, dirtyIds, busyLeafIds } = pending;
  const dirtyCount = dirtyIds.length;
  const busyCount = busyLeafIds.length;
  if (dirtyCount === 1 && busyCount === 0) {
    const dirty = tabs.find(
      (tab) => tab.kind === "editor" && dirtyIds.includes(tab.id),
    );
    return dirty?.title
      ? tr('"{title}" has unsaved changes. Close it anyway?', {
          title: dirty.title,
        })
      : tr("1 tab has unsaved changes. Close it anyway?");
  }
  if (dirtyCount > 0 && busyCount > 0) {
    return tr(
      "{dirtyCount} tabs have unsaved changes and {busyCount} processes are running. Closing will discard the changes and terminate the processes. Close anyway?",
      { dirtyCount, busyCount },
    );
  }
  if (dirtyCount > 0) {
    return tr(
      "{count} tabs have unsaved changes. Closing will discard them. Close anyway?",
      { count: dirtyCount },
    );
  }
  return kind === "right"
    ? tr(
        busyCount === 1
          ? "A process is running in a tab to the right. Closing will terminate it. Close anyway?"
          : "{count} processes are running in tabs to the right. Closing will terminate them. Close anyway?",
        { count: busyCount },
      )
    : tr(
        busyCount === 1
          ? "A process is running in another tab. Closing will terminate it. Close anyway?"
          : "{count} processes are running in other tabs. Closing will terminate them. Close anyway?",
        { count: busyCount },
      );
}

/** Confirmation dialogs for closing dirty editors and terminals with live processes. */
export function CloseDialogs({
  tabs,
  pendingCloseTab,
  onCancelClose,
  onConfirmClose,
  pendingTerminalCloseTab,
  onCancelTerminalClose,
  onConfirmTerminalClose,
  pendingDeleteTabs,
  onCancelDeleteClose,
  onConfirmDeleteClose,
  pendingCloseMany,
  closeManyConfirming,
  onCancelCloseMany,
  onConfirmCloseMany,
  pendingAppClose,
  onCancelAppClose,
  onConfirmAppClose,
}: Props) {
  const tr = useTranslation();
  const [optOutTerminalClose, setOptOutTerminalClose] = useState(false);
  const [optOutAppClose, setOptOutAppClose] = useState(false);
  const appCloseCanOptOut =
    pendingAppClose !== null && canOptOutOfAppClosePrompt(pendingAppClose);

  const confirmTerminalClose = () => {
    if (optOutTerminalClose) void persistOptOut();
    setOptOutTerminalClose(false);
    onConfirmTerminalClose();
  };

  const cancelTerminalClose = () => {
    setOptOutTerminalClose(false);
    onCancelTerminalClose();
  };

  // The pref write has to land before the window closes, or quitting drops it.
  const confirmAppClose = async () => {
    const optOut = appCloseCanOptOut && optOutAppClose;
    setOptOutAppClose(false);
    if (optOut) await persistOptOut();
    onConfirmAppClose();
  };

  const cancelAppClose = () => {
    setOptOutAppClose(false);
    onCancelAppClose();
  };

  return (
    <>
      <AlertDialog
        open={pendingCloseTab !== null}
        onOpenChange={(open) => !open && onCancelClose()}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{tr("Unsaved Changes")}</AlertDialogTitle>
            <AlertDialogDescription>
              {tabs.find((t) => t.id === pendingCloseTab)?.title
                ? tr('"{value0}" has unsaved changes. Close anyway?', {
                    value0: tabs.find((t) => t.id === pendingCloseTab)?.title,
                  })
                : tr("This file has unsaved changes. Close anyway?")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel onClick={onCancelClose}>
              {tr("Cancel")}
            </AlertDialogCancel>
            <AlertDialogAction onClick={onConfirmClose}>
              {tr("Close Anyway")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={pendingTerminalCloseTab !== null}
        onOpenChange={(open) => !open && cancelTerminalClose()}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{tr("Close Terminal?")}</AlertDialogTitle>
            <AlertDialogDescription>
              {tr("A process is running. Closing this tab will terminate it.")}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <OptOutRow
            checked={optOutTerminalClose}
            onCheckedChange={setOptOutTerminalClose}
          />
          <AlertDialogFooter>
            <AlertDialogCancel onClick={cancelTerminalClose}>
              {tr("Cancel")}
            </AlertDialogCancel>
            <AlertDialogAction onClick={confirmTerminalClose}>
              {tr("Close Anyway")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={pendingDeleteTabs !== null}
        onOpenChange={(open) => !open && onCancelDeleteClose()}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{tr("Unsaved Changes")}</AlertDialogTitle>
            <AlertDialogDescription>
              {pendingDeleteTabs?.length === 1
                ? (() => {
                    const title = tabs.find(
                      (t) => t.id === pendingDeleteTabs[0],
                    )?.title;
                    return title
                      ? `"${title}" has unsaved changes. The file has been deleted. Close anyway?`
                      : "This file has unsaved changes. The file has been deleted. Close anyway?";
                  })()
                : tr(
                    "{value0} files have unsaved changes. They have been deleted. Close all anyway?",
                    { value0: pendingDeleteTabs?.length ?? 0 },
                  )}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel onClick={onCancelDeleteClose}>
              {tr("Cancel")}
            </AlertDialogCancel>
            <AlertDialogAction onClick={onConfirmDeleteClose}>
              {tr("Close Anyway")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={pendingCloseMany !== null}
        onOpenChange={(open) => !open && onCancelCloseMany()}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {pendingCloseMany?.kind === "right"
                ? tr("Close Tabs to the Right")
                : tr("Close Other Tabs")}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {pendingCloseMany
                ? closeManyMessage(pendingCloseMany, tabs, tr)
                : ""}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel onClick={onCancelCloseMany}>
              {tr("Cancel")}
            </AlertDialogCancel>
            <AlertDialogAction
              disabled={closeManyConfirming}
              onClick={(event) => {
                event.preventDefault();
                onConfirmCloseMany();
              }}
            >
              {closeManyConfirming ? tr("Checking...") : tr("Close Anyway")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={pendingAppClose !== null}
        onOpenChange={(open) => !open && cancelAppClose()}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{tr("Quit RCode?")}</AlertDialogTitle>
            <AlertDialogDescription>
              {pendingAppClose ? appCloseMessage(pendingAppClose, tr) : ""}
            </AlertDialogDescription>
          </AlertDialogHeader>
          {appCloseCanOptOut ? (
            <OptOutRow
              checked={optOutAppClose}
              onCheckedChange={setOptOutAppClose}
            />
          ) : null}
          <AlertDialogFooter>
            <AlertDialogCancel onClick={cancelAppClose}>
              {tr("Cancel")}
            </AlertDialogCancel>
            <AlertDialogAction onClick={() => void confirmAppClose()}>
              {tr("Quit Anyway")}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
