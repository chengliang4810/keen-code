import { Button } from "@/components/ui/button";
import { useEffect, useMemo, useRef, useState } from "react";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import { GlassModal } from "@/components/GlassModal";
import { diagnosticsLogPath } from "@/lib/acp/api";
import { pathOpen, pathReveal } from "@/lib/api";
import { localizeUiError } from "@/lib/session";

/** 诊断日志支持的三个系统操作。 */
export type StatusModalDiagnosticsAction = "open" | "reveal" | "copy";

/** 执行诊断日志路径操作；错误交由 Modal 保持可见并展示。 */
export async function performStatusModalDiagnosticsAction(
  action: StatusModalDiagnosticsAction,
  path: string,
): Promise<void> {
  if (action === "open") {
    await pathOpen(path);
    return;
  }
  if (action === "reveal") {
    await pathReveal(path);
    return;
  }
  await navigator.clipboard.writeText(path);
}

/** 展示当前会话元数据及后端诊断日志操作的状态弹窗。 */
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

  /** 后端返回的诊断日志绝对路径。 */
  const [diagnosticsPath, setDiagnosticsPath] = useState<string | null>(null);
  /** 诊断日志路径是否正在首次加载。 */
  const [diagnosticsLoading, setDiagnosticsLoading] = useState(false);
  /** 诊断路径读取或操作的原始异常，渲染时按当前语言转换。 */
  const [diagnosticsError, setDiagnosticsError] = useState<unknown>(null);
  /** 当前正在执行的诊断路径操作，避免重复触发系统调用。 */
  const [diagnosticsAction, setDiagnosticsAction] =
    useState<StatusModalDiagnosticsAction | null>(null);
  /** 最近一次复制路径是否成功，用于给出可见反馈。 */
  const [diagnosticsCopied, setDiagnosticsCopied] = useState(false);
  /** 诊断弹窗当前打开周期的序号，隔离关闭后迟到的异步结果。 */
  const diagnosticsGeneration = useRef(0);
  /** 诊断弹窗当前是否仍处于可接收异步结果的打开周期。 */
  const diagnosticsActive = useRef(false);

  /** 仅在 Modal 打开时读取一次诊断路径，不建立常驻轮询。 */
  useEffect(() => {
    const generation = diagnosticsGeneration.current + 1;
    diagnosticsGeneration.current = generation;
    diagnosticsActive.current = open;
    if (!open) {
      return () => {
        if (diagnosticsGeneration.current === generation) {
          diagnosticsActive.current = false;
        }
      };
    }
    let active = true;
    setDiagnosticsPath(null);
    setDiagnosticsLoading(true);
    setDiagnosticsError(null);
    setDiagnosticsAction(null);
    setDiagnosticsCopied(false);
    let request: Promise<string>;
    try {
      request = diagnosticsLogPath();
    } catch (cause) {
      setDiagnosticsError(cause);
      setDiagnosticsLoading(false);
      return () => {
        active = false;
        if (diagnosticsGeneration.current === generation) {
          diagnosticsActive.current = false;
        }
      };
    }
    void request
      .then((path) => {
        if (!active) return;
        /** 去除 IPC 返回路径两端空白，避免把无效路径交给系统操作。 */
        const normalizedPath = path.trim();
        if (normalizedPath) {
          setDiagnosticsPath(normalizedPath);
        } else {
          setDiagnosticsError(new Error("诊断日志路径为空"));
        }
      })
      .catch((cause) => {
        if (active) setDiagnosticsError(cause);
      })
      .finally(() => {
        if (active) setDiagnosticsLoading(false);
      });
    return () => {
      active = false;
      if (diagnosticsGeneration.current === generation) {
        diagnosticsActive.current = false;
      }
    };
  }, [open]);

  /** 执行日志操作并把失败留在当前 Modal 中，不触发关闭。 */
  const runDiagnosticsAction = async (
    action: StatusModalDiagnosticsAction,
  ) => {
    const path = diagnosticsPath;
    const generation = diagnosticsGeneration.current;
    const isCurrent = () =>
      diagnosticsActive.current && diagnosticsGeneration.current === generation;
    if (!isCurrent() || !path || diagnosticsLoading || diagnosticsAction) return;
    setDiagnosticsAction(action);
    setDiagnosticsError(null);
    setDiagnosticsCopied(false);
    try {
      await performStatusModalDiagnosticsAction(action, path);
      if (isCurrent() && action === "copy") setDiagnosticsCopied(true);
    } catch (cause) {
      if (isCurrent()) setDiagnosticsError(cause);
    } finally {
      if (isCurrent()) setDiagnosticsAction(null);
    }
  };

  /** 日志路径加载或系统操作期间禁用其它诊断按钮。 */
  const diagnosticsBusy = diagnosticsLoading || diagnosticsAction !== null;
  /** 首次打开且尚未收到结果时展示加载状态，避免短暂空状态闪烁。 */
  const diagnosticsWaiting =
    diagnosticsLoading ||
    (open && !diagnosticsPath && diagnosticsError === null);

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
        <div className="status-modal__row">
          <dt>{tr("statusModal.logPath")}</dt>
          <dd
            title={diagnosticsPath ?? undefined}
            aria-live="polite"
            aria-atomic="true"
          >
            {diagnosticsPath ? (
              <code className="status-modal__log-path" title={diagnosticsPath}>
                {diagnosticsPath}
              </code>
            ) : diagnosticsWaiting ? (
              tr("statusModal.diagnosticsLoading")
            ) : (
              tr("statusModal.diagnosticsUnavailable")
            )}
          </dd>
        </div>
      </dl>
      <section
        className="status-modal__diagnostics"
        aria-labelledby="status-modal-diagnostics-title"
      >
        <h3 id="status-modal-diagnostics-title" className="modal-title">
          {tr("statusModal.diagnosticsTitle")}
        </h3>
        <div className="status-modal__diagnostics-actions">
          <Button
            type="button"
            className="btn btn--ghost btn--sm"
            disabled={!diagnosticsPath || diagnosticsBusy}
            aria-busy={diagnosticsBusy}
            onClick={() => void runDiagnosticsAction("open")}
          >
            {tr("statusModal.openLog")}
          </Button>
          <Button
            type="button"
            className="btn btn--ghost btn--sm"
            disabled={!diagnosticsPath || diagnosticsBusy}
            aria-busy={diagnosticsBusy}
            onClick={() => void runDiagnosticsAction("reveal")}
          >
            {tr("statusModal.revealLog")}
          </Button>
          <Button
            type="button"
            className="btn btn--ghost btn--sm"
            disabled={!diagnosticsPath || diagnosticsBusy}
            aria-busy={diagnosticsBusy}
            onClick={() => void runDiagnosticsAction("copy")}
          >
            {tr("statusModal.copyLogPath")}
          </Button>
        </div>
        {diagnosticsError ? (
          <p className="status-modal__diagnostics-error" role="alert">
            {localizeUiError(diagnosticsError, locale)}
          </p>
        ) : null}
        {diagnosticsCopied ? (
          <p role="status" aria-live="polite">
            {tr("statusModal.pathCopied")}
          </p>
        ) : null}
      </section>
    </GlassModal>
  );
}
