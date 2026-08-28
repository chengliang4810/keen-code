import type { Locale } from "@/i18n";
import type { AppUpdateStatus } from "@/lib/api";
import type { AppUpdateBusy } from "@/components/AppUpdateSection";
import { AppUpdateProgress } from "@/components/AppUpdateProgress";
import { GlassModal } from "@/components/GlassModal";
import type { AsyncAction, SetState, Translator } from "./types";

export interface AppUpdateModalProps {
  tr: Translator;
  locale: Locale;
  open: boolean;
  setOpen: SetState<boolean>;
  status: AppUpdateStatus | null;
  busy: AppUpdateBusy;
  error: string | null;
  check: AsyncAction;
  install: AsyncAction;
}

export function AppUpdateModal({
  tr,
  locale,
  open,
  setOpen,
  status,
  busy,
  error,
  check,
  install,
}: AppUpdateModalProps) {
  return (
    <GlassModal
      open={open}
      onClose={() => setOpen(false)}
      title={tr("settings.updateTitle")}
      size="sm"
      closeLabel={tr("common.close")}
      closeOnOverlay={false}
      wrapBody
    >
      <AppUpdateProgress
        locale={locale}
        status={status}
        installing={busy === "installing"}
        error={error}
        onRetry={check}
        onInstall={install}
      />
    </GlassModal>
  );
}
