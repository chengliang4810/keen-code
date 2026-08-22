import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Button } from "@/components/ui/button";
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  requestRecordsList,
  type RequestRecord,
  type RequestRecordsPage,
  type RequestRecordsQuery,
} from "@/lib/api";
import { formatTokenCount } from "@/lib/contextUsage";
import type { Locale } from "@/i18n";
import { GlassModal } from "@/components/GlassModal";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

export const REQUEST_HISTORY_PAGE_SIZE = 20;

export type RequestHistoryFilters = {
  model: string;
  status: string;
  from: string;
  to: string;
};

export type RequestHistoryLabels = {
  loading: string;
  error: string;
  empty: string;
  refresh: string;
  refreshing: string;
  invalidDateRange: string;
  filters: string;
  model: string;
  status: string;
  from: string;
  to: string;
  allModels: string;
  allStatuses: string;
  clearFilters: string;
  time: string;
  provider: string;
  requestMode: string;
  stream: string;
  sync: string;
  attempt: string;
  duration: string;
  tokens: string;
  details: string;
  close: string;
  purpose: string;
  protocol: string;
  endpoint: string;
  logicalRequestId: string;
  sessionId: string;
  turnId: string;
  agentId: string;
  firstResponse: string;
  firstResponseDuration: string;
  completedAt: string;
  httpStatus: string;
  providerRequestId: string;
  errorKind: string;
  errorDetail: string;
  cacheCreation: string;
  cacheRead: string;
  inputTokens: string;
  outputTokens: string;
  notReported: string;
  previous: string;
  next: string;
  range: string;
  statusSuccess: string;
  statusRunning: string;
  statusFailed: string;
  statusCancelled: string;
  statusConnection: string;
  statusTimeout: string;
  statusTls: string;
  statusTransport: string;
  statusHttp: string;
  statusProtocol: string;
  statusStreamInterrupted: string;
  statusRetryExhausted: string;
  statusOther: string;
};

type Props = {
  locale: Locale;
  labels: RequestHistoryLabels;
};

const EMPTY_FILTERS: RequestHistoryFilters = {
  model: "",
  status: "",
  from: "",
  to: "",
};

// Radix Select reserves an empty string for clearing the current value. Encode
// every real option so the dedicated "all" value cannot collide with a model
// or status returned by the backend.
export type RequestHistorySelectKind = "model" | "status";

export function encodeRequestHistorySelectValue(
  kind: RequestHistorySelectKind,
  value: string,
): string {
  return `${kind}:${value}`;
}

export function decodeRequestHistorySelectValue(
  kind: RequestHistorySelectKind,
  value: string,
): string {
  const prefix = `${kind}:`;
  return value.startsWith(prefix) ? value.slice(prefix.length) : value;
}

const ALL_MODELS_VALUE = encodeRequestHistorySelectValue("model", "");
const ALL_STATUSES_VALUE = encodeRequestHistorySelectValue("status", "");

function parseDateInput(value: string): Date | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!match) return null;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const date = new Date(year, month - 1, day);
  if (
    date.getFullYear() !== year ||
    date.getMonth() !== month - 1 ||
    date.getDate() !== day
  ) {
    return null;
  }
  return date;
}

/** Convert a local date input to an inclusive Unix-millisecond filter bound. */
export function requestHistoryDateToMs(
  value: string,
  endOfDay = false,
): number | undefined {
  const date = parseDateInput(value);
  if (!date) return undefined;
  if (endOfDay) {
    date.setDate(date.getDate() + 1);
    return date.getTime() - 1;
  }
  return date.getTime();
}

/** Build the exact backend query from visible filters and the current page. */
export function buildRequestHistoryQuery(
  filters: RequestHistoryFilters,
  offset: number,
  limit = REQUEST_HISTORY_PAGE_SIZE,
): RequestRecordsQuery {
  const query: RequestRecordsQuery = {
    offset: Math.max(0, Math.floor(offset)),
    limit: Math.max(1, Math.floor(limit)),
  };
  if (filters.model) query.model = filters.model;
  if (filters.status) query.status = filters.status;
  const fromMs = requestHistoryDateToMs(filters.from);
  if (fromMs != null) query.fromMs = fromMs;
  const toMs = requestHistoryDateToMs(filters.to, true);
  if (toMs != null) query.toMs = toMs;
  return query;
}

export function formatRequestHistoryMode(
  mode: RequestRecord["requestMode"],
  labels: Pick<RequestHistoryLabels, "stream" | "sync">,
): string {
  return mode === "stream" ? labels.stream : labels.sync;
}

export function formatRequestHistoryTime(
  atMs: number | null | undefined,
  locale: Locale,
  missing: string,
): string {
  if (atMs == null || !Number.isFinite(atMs)) return missing;
  const date = new Date(atMs);
  return Number.isNaN(date.getTime()) ? missing : date.toLocaleString(locale);
}

/** Strip query/hash fragments before showing an endpoint in the safe details view. */
export function formatSafeRequestEndpoint(
  endpoint: string | null | undefined,
  missing: string,
): string {
  const value = endpoint?.trim();
  if (!value) return missing;
  try {
    const url = new URL(value);
    url.username = "";
    url.password = "";
    url.search = "";
    url.hash = "";
    return url.toString();
  } catch {
    return value.split(/[?#]/, 1)[0] || missing;
  }
}

function statusClass(status: string): string {
  const normalized = status.trim().toLowerCase().replace(/[^a-z0-9_-]+/g, "-");
  return normalized ? `request-history-status--${normalized}` : "";
}

export function formatRequestHistoryStatus(
  status: string,
  labels: RequestHistoryLabels,
): string {
  const known: Record<string, string> = {
    success: labels.statusSuccess,
    running: labels.statusRunning,
    failed: labels.statusFailed,
    cancelled: labels.statusCancelled,
    connection: labels.statusConnection,
    timeout: labels.statusTimeout,
    tls: labels.statusTls,
    transport: labels.statusTransport,
    http_status: labels.statusHttp,
    protocol: labels.statusProtocol,
    stream_interrupted: labels.statusStreamInterrupted,
    retry_exhausted: labels.statusRetryExhausted,
    other: labels.statusOther,
  };
  const normalized = status.trim().toLowerCase();
  return known[normalized] ?? status;
}

function formatElapsedMs(
  startedAtMs: number,
  endedAtMs: number | null | undefined,
  locale: Locale,
  missing: string,
): string {
  if (endedAtMs == null || !Number.isFinite(endedAtMs)) return missing;
  return `${Math.max(0, endedAtMs - startedAtMs).toLocaleString(locale)} ms`;
}

function displayValue(value: string | null | undefined, missing: string): string {
  const trimmed = value?.trim();
  return trimmed || missing;
}

export function formatRequestHistoryTokens(
  record: Pick<
    RequestRecord,
    "usageReported" | "inputTokens" | "outputTokens" | "estimated"
  >,
  missing: string,
): string {
  if (!record.usageReported) return missing;
  const total = Math.max(0, record.inputTokens) + Math.max(0, record.outputTokens);
  return `${record.estimated ? "~" : ""}${formatTokenCount(total)}`;
}

function DetailRow({ label, value, error = false }: { label: string; value: string; error?: boolean }) {
  return (
    <>
      <dt>{label}</dt>
      <dd className={error ? "request-history__detail-error" : undefined}>{value}</dd>
    </>
  );
}

export type RequestHistoryDetailRow = {
  label: string;
  value: string;
  error?: boolean;
};

/** Build the complete, safe projection shown in the request details dialog. */
export function buildRequestHistoryDetailRows(
  record: RequestRecord,
  locale: Locale,
  labels: RequestHistoryLabels,
): RequestHistoryDetailRow[] {
  const rows: RequestHistoryDetailRow[] = [
    {
      label: labels.time,
      value: formatRequestHistoryTime(
        record.requestedAtMs,
        locale,
        labels.notReported,
      ),
    },
    { label: labels.model, value: record.model },
    {
      label: labels.provider,
      value: displayValue(record.provider, labels.notReported),
    },
    {
      label: labels.requestMode,
      value: formatRequestHistoryMode(record.requestMode, labels),
    },
    {
      label: labels.status,
      value: formatRequestHistoryStatus(
        displayValue(record.status, labels.notReported),
        labels,
      ),
    },
    {
      label: labels.attempt,
      value: `${record.attempt}/${Math.max(1, record.maxAttempts)}`,
    },
    {
      label: labels.duration,
      value: `${Math.max(0, record.durationMs).toLocaleString(locale)} ms`,
    },
    {
      label: labels.tokens,
      value: formatRequestHistoryTokens(record, labels.notReported),
    },
    {
      label: labels.purpose,
      value: displayValue(record.purpose, labels.notReported),
    },
    {
      label: labels.protocol,
      value: displayValue(record.protocol, labels.notReported),
    },
    {
      label: labels.endpoint,
      value: formatSafeRequestEndpoint(record.endpoint, labels.notReported),
    },
    { label: labels.logicalRequestId, value: record.logicalRequestId },
    {
      label: labels.sessionId,
      value: displayValue(record.sessionId, labels.notReported),
    },
    {
      label: labels.turnId,
      value: displayValue(record.turnId, labels.notReported),
    },
    {
      label: labels.agentId,
      value: displayValue(record.agentId, labels.notReported),
    },
    {
      label: labels.firstResponse,
      value: formatRequestHistoryTime(
        record.firstResponseAtMs,
        locale,
        labels.notReported,
      ),
    },
    {
      label: labels.firstResponseDuration,
      value: formatElapsedMs(
        record.requestedAtMs,
        record.firstResponseAtMs,
        locale,
        labels.notReported,
      ),
    },
    {
      label: labels.completedAt,
      value: formatRequestHistoryTime(
        record.completedAtMs,
        locale,
        labels.notReported,
      ),
    },
    {
      label: labels.httpStatus,
      value:
        record.httpStatus == null
          ? labels.notReported
          : String(record.httpStatus),
    },
    {
      label: labels.providerRequestId,
      value: displayValue(record.providerRequestId, labels.notReported),
    },
    {
      label: labels.inputTokens,
      value: record.usageReported
        ? formatTokenCount(record.inputTokens)
        : labels.notReported,
    },
    {
      label: labels.outputTokens,
      value: record.usageReported
        ? formatTokenCount(record.outputTokens)
        : labels.notReported,
    },
    {
      label: labels.cacheCreation,
      value:
        record.cacheCreationTokens == null
          ? labels.notReported
          : formatTokenCount(record.cacheCreationTokens),
    },
    {
      label: labels.cacheRead,
      value:
        record.cacheReadTokens == null
          ? labels.notReported
          : formatTokenCount(record.cacheReadTokens),
    },
  ];
  if (record.errorKind) {
    rows.push({
      label: labels.errorKind,
      value: formatRequestHistoryStatus(record.errorKind, labels),
      error: true,
    });
  }
  if (record.error) {
    rows.push({ label: labels.errorDetail, value: record.error, error: true });
  }
  return rows;
}

export function RequestHistoryPanel({ locale, labels }: Props) {
  const [filters, setFilters] = useState<RequestHistoryFilters>(EMPTY_FILTERS);
  const [offset, setOffset] = useState(0);
  const [page, setPage] = useState<RequestRecordsPage | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [refreshVersion, setRefreshVersion] = useState(0);
  const [selectedRecord, setSelectedRecord] = useState<RequestRecord | null>(null);

  const closeDetails = useCallback(() => setSelectedRecord(null), []);

  const query = useMemo(
    () => buildRequestHistoryQuery(filters, offset),
    [filters, offset],
  );

  useEffect(() => {
    let active = true;
    if (
      query.fromMs != null &&
      query.toMs != null &&
      query.fromMs > query.toMs
    ) {
      setLoading(false);
      setError(labels.invalidDateRange);
      return () => {
        active = false;
      };
    }
    setLoading(true);
    setError(null);
    void requestRecordsList(query)
      .then((result) => {
        if (!active) return;
        setPage(result);
      })
      .catch((cause) => {
        if (!active) return;
        setError(cause instanceof Error ? cause.message : String(cause));
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [labels.invalidDateRange, query, refreshVersion]);

  const updateFilter = useCallback(
    (key: keyof RequestHistoryFilters, value: string) => {
      setOffset(0);
      setFilters((previous) => ({ ...previous, [key]: value }));
    },
    [],
  );

  const clearFilters = useCallback(() => {
    if (
      offset === 0 &&
      !filters.model &&
      !filters.status &&
      !filters.from &&
      !filters.to
    ) {
      return;
    }
    setOffset(0);
    setFilters(EMPTY_FILTERS);
  }, [filters, offset]);

  const modelOptions = useMemo(() => {
    const values = new Set(page?.models ?? []);
    if (filters.model) values.add(filters.model);
    return [...values].sort((a, b) => a.localeCompare(b));
  }, [filters.model, page?.models]);

  const statusOptions = useMemo(() => {
    const values = new Set(page?.statuses ?? []);
    if (filters.status) values.add(filters.status);
    return [...values].sort((a, b) => a.localeCompare(b));
  }, [filters.status, page?.statuses]);

  const currentPage = page ?? {
    records: [],
    total: 0,
    offset,
    limit: REQUEST_HISTORY_PAGE_SIZE,
    hasMore: false,
    models: [],
    statuses: [],
  };
  const first = currentPage.records.length === 0 ? 0 : currentPage.offset + 1;
  const last = Math.min(currentPage.offset + currentPage.records.length, currentPage.total);

  return (
    <div className="request-history" id="settings-anchor-requests" aria-busy={loading}>
      <div className="request-history__filters-card">
        <div className="request-history__filters-heading">
          <h2>{labels.filters}</h2>
          <div className="request-history__filter-actions">
            <Button
              type="button"
              className="request-history__clear"
              disabled={loading}
              onClick={() => setRefreshVersion((current) => current + 1)}
            >
              {loading ? labels.refreshing : labels.refresh}
            </Button>
            <Button type="button" className="request-history__clear" onClick={clearFilters}>
              {labels.clearFilters}
            </Button>
          </div>
        </div>
        <div className="request-history__filters">
          <Label>
            <span>{labels.model}</span>
            <Select
              value={
                filters.model
                  ? encodeRequestHistorySelectValue("model", filters.model)
                  : ALL_MODELS_VALUE
              }
              onValueChange={(value) =>
                updateFilter(
                  "model",
                  decodeRequestHistorySelectValue("model", value),
                )
              }
            >
              <SelectTrigger
                className="request-history__filter-select"
                aria-label={labels.model}
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  <SelectItem value={ALL_MODELS_VALUE}>{labels.allModels}</SelectItem>
                  {modelOptions.map((model) => (
                    <SelectItem
                      key={model}
                      value={encodeRequestHistorySelectValue("model", model)}
                    >
                      {model}
                    </SelectItem>
                  ))}
                </SelectGroup>
              </SelectContent>
            </Select>
          </Label>
          <Label>
            <span>{labels.status}</span>
            <Select
              value={
                filters.status
                  ? encodeRequestHistorySelectValue("status", filters.status)
                  : ALL_STATUSES_VALUE
              }
              onValueChange={(value) =>
                updateFilter(
                  "status",
                  decodeRequestHistorySelectValue("status", value),
                )
              }
            >
              <SelectTrigger
                className="request-history__filter-select"
                aria-label={labels.status}
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  <SelectItem value={ALL_STATUSES_VALUE}>{labels.allStatuses}</SelectItem>
                  {statusOptions.map((status) => (
                    <SelectItem
                      key={status}
                      value={encodeRequestHistorySelectValue("status", status)}
                    >
                      {formatRequestHistoryStatus(status, labels)}
                    </SelectItem>
                  ))}
                </SelectGroup>
              </SelectContent>
            </Select>
          </Label>
          <Label>
            <span>{labels.from}</span>
            <Input type="date" value={filters.from} onChange={(event) => updateFilter("from", event.target.value)} />
          </Label>
          <Label>
            <span>{labels.to}</span>
            <Input type="date" value={filters.to} onChange={(event) => updateFilter("to", event.target.value)} />
          </Label>
        </div>
      </div>

      {error ? <div className="request-history__error" role="alert">{labels.error}: {error}</div> : null}

      {loading && !page ? (
        <div className="analytics-empty">{labels.loading}</div>
      ) : !currentPage.records.length ? (
        <div className="analytics-empty">{labels.empty}</div>
      ) : (
        <div className="request-history__table-wrap">
          <table className="request-history__table">
            <thead>
              <tr>
                <th>{labels.time}</th>
                <th>{labels.model}</th>
                <th>{labels.provider}</th>
                <th>{labels.requestMode}</th>
                <th>{labels.status}</th>
                <th>{labels.attempt}</th>
                <th>{labels.duration}</th>
                <th>{labels.tokens}</th>
                <th>{labels.details}</th>
              </tr>
            </thead>
            <tbody>
              {currentPage.records.map((record) => (
                <tr key={record.id}>
                  <td>{formatRequestHistoryTime(record.requestedAtMs, locale, labels.notReported)}</td>
                  <td className="request-history__model">{record.model}</td>
                  <td>{displayValue(record.provider, labels.notReported)}</td>
                  <td>{formatRequestHistoryMode(record.requestMode, labels)}</td>
                  <td><span className={`request-history__status ${statusClass(record.status)}`}>{formatRequestHistoryStatus(displayValue(record.status, labels.notReported), labels)}</span></td>
                  <td>{record.attempt}/{Math.max(1, record.maxAttempts)}</td>
                  <td>{Math.max(0, record.durationMs).toLocaleString(locale)} ms</td>
                  <td>{formatRequestHistoryTokens(record, labels.notReported)}</td>
                  <td>
                    <Button
                      type="button"
                      className="request-history__details-trigger"
                      aria-haspopup="dialog"
                      aria-label={`${labels.details}: ${record.model}`}
                      onClick={() => setSelectedRecord(record)}
                    >
                      {labels.details}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <div className="request-history__pagination">
        <span>{labels.range.replace("{from}", String(first)).replace("{to}", String(last)).replace("{total}", String(currentPage.total))}</span>
        <div>
          <Button type="button" disabled={currentPage.offset <= 0 || loading} onClick={() => setOffset(Math.max(0, currentPage.offset - currentPage.limit))}>{labels.previous}</Button>
          <Button type="button" disabled={!currentPage.hasMore || loading} onClick={() => setOffset(currentPage.offset + currentPage.limit)}>{labels.next}</Button>
        </div>
      </div>

      <GlassModal
        open={selectedRecord != null}
        onClose={closeDetails}
        title={labels.details}
        titleId="request-history-details-title"
        closeLabel={labels.close}
        size="lg"
        className="request-history__details-modal"
        bodyClassName="request-history__details-modal-body"
        wrapBody
        footer={
          <Button type="button" className="btn btn--solid" onClick={closeDetails}>
            {labels.close}
          </Button>
        }
      >
        {selectedRecord ? (
          <dl>
            {buildRequestHistoryDetailRows(selectedRecord, locale, labels).map(
              (row) => (
                <DetailRow
                  key={row.label}
                  label={row.label}
                  value={row.value}
                  error={row.error}
                />
              ),
            )}
          </dl>
        ) : null}
      </GlassModal>
    </div>
  );
}
