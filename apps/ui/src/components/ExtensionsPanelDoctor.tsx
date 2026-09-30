/** MCP Doctor 诊断对话框；运行状态自持，由外部请求触发。 */

import { useCallback, useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Alert, AlertDescription } from "@appica/ui-react/alert";
import { createT, type Locale } from "@/i18n";
import { localizeUiError } from "@/lib/session";
import * as api from "@/lib/api";
import { Badge } from "@appica/ui-react/badge";
import { GlassModal } from "@/components/GlassModal";
import { IconRefresh } from "@/components/icons";
import { shortPathLabel } from "@/lib/extensionsUi";

/** 一次诊断请求；`focus` 限定单个服务器，`null` 为全量诊断。 */
export interface McpDoctorRequest {
  /** 聚焦诊断的服务器名。 */
  focus: string | null;
}

/** `McpDoctorDialog` 的属性。 */
export interface McpDoctorDialogProps {
  /** 本地化语言。 */
  locale: Locale;
  /** 当前项目根；诊断按项目作用域执行。 */
  projectPath: string | null;
  /** 非空时打开并按其 focus 运行诊断；置空关闭对话框。 */
  request: McpDoctorRequest | null;
  /** 对话框关闭回调；父组件应把 request 置空。 */
  onClose: () => void;
}

/** 运行 MCP Doctor 并展示健康报告的模态对话框。 */
export function McpDoctorDialog({
  locale,
  projectPath,
  request,
  onClose,
}: McpDoctorDialogProps) {
  const tr = createT(locale);
  const [loading, setLoading] = useState(false);
  const [report, setReport] = useState<api.McpDoctorReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [focus, setFocus] = useState<string | null>(null);

  const run = useCallback(
    async (focusName?: string | null) => {
      if (!api.isTauri()) return;
      setLoading(true);
      setError(null);
      setFocus(focusName?.trim() || null);
      try {
        const result = await api.mcpDoctor(
          focusName?.trim() || null,
          projectPath?.trim() || null,
        );
        setReport(result);
      } catch (e) {
        setReport(null);
        setError(localizeUiError(e, locale));
      } finally {
        setLoading(false);
      }
    },
    [locale, projectPath],
  );

  // 每次新的诊断请求打开对话框并立即运行；请求置空只负责关闭。
  useEffect(() => {
    if (!request) return;
    void run(request.focus);
  }, [request, run]);

  return (
    <GlassModal
      open={!!request}
      onClose={() => {
        if (!loading) onClose();
      }}
      title={
        focus ? `${tr("ext.mcp.doctorTitle")} · ${focus}` : tr("ext.mcp.doctorTitle")
      }
      size="md"
      closeLabel={tr("common.close")}
      wrapBody
      footer={
        <>
          <Button size="md"
            type="button"
            variant="ghost"
            disabled={loading}
            onClick={() => void run(focus)}
          >
            <IconRefresh size={14} />
            <span>{tr("ext.mcp.doctorRerun")}</span>
          </Button>
          <Button size="md"
            type="button"
            variant="ghost"
            disabled={loading}
            onClick={onClose}
          >
            {tr("common.close")}
          </Button>
        </>
      }
    >
      {loading && <p className="ext-empty">{tr("ext.mcp.doctorRunning")}</p>}
      {!loading && error && (
        <Alert variant="error">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
      {!loading && report && (
        <div className="ext-doctor">
          <p className="ext-doctor__summary">
            {tr("ext.mcp.doctorSummary", {
              healthy: report.summary.healthy,
              unhealthy: report.summary.unhealthy,
              total: report.summary.total,
            })}
          </p>
          {report.sources.length > 0 ? (
            <div className="ext-doctor__sources">
              <div className="ext-doctor__section-title">
                {tr("ext.mcp.doctorSources")}
              </div>
              <ul className="ext-doctor__source-list">
                {report.sources.map((src) => (
                  <li key={src.path}>
                    <code>{src.path}</code>
                    <Badge size="md" variant="soft">
                      {src.status} · {src.serverCount}
                    </Badge>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
          {report.servers.length === 0 ? (
            <p className="ext-empty">
              {report.rawText?.trim() || tr("ext.mcp.doctorEmpty")}
            </p>
          ) : (
            <ul className="ext-list ext-doctor__servers">
              {report.servers.map((s) => (
                <li key={s.name} className={"ext-item" + (s.healthy ? "" : " ext-item--off")}>
                  <div className="ext-item__head">
                    <strong className="ext-item__name">{s.name}</strong>
                    <Badge size="md" variant={s.healthy ? "success" : "error"}>
                      {s.healthy
                        ? tr("ext.mcp.doctorHealthy")
                        : tr("ext.mcp.doctorUnhealthy")}
                    </Badge>
                    <Badge size="md" variant="soft">
                      {s.transport}
                    </Badge>
                  </div>
                  {s.target ? (
                    <p className="ext-item__desc" title={s.target}>
                      {shortPathLabel(s.target, 72)}
                    </p>
                  ) : null}
                  {s.checks.length > 0 ? (
                    <ul className="ext-doctor__checks">
                      {s.checks.map((c, index) => (
                        <li
                          key={`${s.name}:${c.label}:${index}`}
                          className={
                            "ext-doctor__check" + (c.passed ? " is-pass" : " is-fail")
                          }
                        >
                          <span className="ext-doctor__check-label">
                            {c.passed ? "✓" : "✗"} {c.label}
                          </span>
                          {c.detail ? (
                            <span className="ext-doctor__check-detail">
                              {c.detail}
                            </span>
                          ) : null}
                        </li>
                      ))}
                    </ul>
                  ) : null}
                </li>
              ))}
            </ul>
          )}
          {report.rawText ? <pre className="ext-details-pre">{report.rawText}</pre> : null}
        </div>
      )}
    </GlassModal>
  );
}
