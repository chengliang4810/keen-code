import { Button } from "@/components/ui/button";
import { useMemo } from "react";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import { GlassModal } from "@/components/GlassModal";

export function StatusModal({
  open,
  locale,
  sessionId,
  modelId,
  effort,
  projectPath,
  messageCount,
  onClose,
}: {
  open: boolean;
  locale: Locale;
  sessionId?: string | null;
  modelId?: string | null;
  effort?: string | null;
  projectPath?: string | null;
  messageCount?: number;
  onClose: () => void;
}) {
  const tr = useMemo(() => createT(locale), [locale]);

  const rows: { label: string; value: string }[] = [
    { label: tr("statusModal.sessionId"), value: sessionId || "—" },
    { label: tr("statusModal.model"), value: modelId || "—" },
    { label: tr("statusModal.effort"), value: effort || "—" },
    {
      label: tr("statusModal.executionPolicy"),
      value: tr("statusModal.executionPolicyValue"),
    },
    { label: tr("statusModal.project"), value: projectPath || "—" },
    {
      label: tr("statusModal.messages"),
      value: String(messageCount ?? 0),
    },
  ];

  return (
    <GlassModal
      open={open}
      onClose={onClose}
      title={tr("statusModal.title")}
      titleId="status-modal-title"
      closeLabel={tr("common.close")}
      size="md"
      className="status-modal"
      footer={
        <Button type="button" className="btn btn--solid" onClick={onClose}>
          {tr("common.close")}
        </Button>
      }
    >
      <dl className="status-modal__dl">
        {rows.map((r) => (
          <div key={r.label} className="status-modal__row">
            <dt>{r.label}</dt>
            <dd title={r.value}>{r.value}</dd>
          </div>
        ))}
      </dl>
    </GlassModal>
  );
}
