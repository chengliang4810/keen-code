import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import type { RequestRecord } from "@/lib/api";
import {
  REQUEST_HISTORY_PAGE_SIZE,
  type RequestHistoryLabels,
  buildRequestHistoryDetailRows,
  buildRequestHistoryQuery,
  decodeRequestHistorySelectValue,
  encodeRequestHistorySelectValue,
  formatRequestHistoryTokens,
  formatSafeRequestEndpoint,
  requestHistoryDateToMs,
} from "./RequestHistoryPanel";

describe("RequestHistoryPanel query projection", () => {
  it("以 20 条为默认分页，并把模型、状态和本地日期映射到后端查询", () => {
    const query = buildRequestHistoryQuery(
      {
        model: "gpt-test",
        status: "failed",
        from: "2026-01-01",
        to: "2026-01-03",
      },
      20,
    );

    expect(query.offset).toBe(20);
    expect(query.limit).toBe(REQUEST_HISTORY_PAGE_SIZE);
    expect(query.model).toBe("gpt-test");
    expect(query.status).toBe("failed");
    expect(query.fromMs).toBe(requestHistoryDateToMs("2026-01-01"));
    expect(query.toMs).toBe(requestHistoryDateToMs("2026-01-03", true));
    expect(query.toMs).toBeGreaterThan(query.fromMs ?? 0);
  });

  it("忽略空筛选和无效日期，不向后端伪造边界", () => {
    expect(buildRequestHistoryQuery({ model: "", status: "", from: "", to: "" }, 0)).toEqual({
      offset: 0,
      limit: REQUEST_HISTORY_PAGE_SIZE,
    });
    expect(requestHistoryDateToMs("2026-02-30")).toBeUndefined();
    expect(buildRequestHistoryQuery({ model: "", status: "", from: "bad", to: "2026-02-30" }, 0)).toEqual({
      offset: 0,
      limit: REQUEST_HISTORY_PAGE_SIZE,
    });
  });

  it("分页偏移量与筛选独立，下一页仍固定请求 20 条", () => {
    const query = buildRequestHistoryQuery(
      { model: "", status: "success", from: "", to: "" },
      40,
    );
    expect(query).toEqual({ offset: 40, limit: 20, status: "success" });
  });

  it("下拉筛选器使用分组 Select，并保留空筛选与带前缀选项的边界", () => {
    const source = readFileSync(
      new URL("./RequestHistoryPanel.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain('from "@/components/ui/select"');
    expect(source).not.toMatch(/<select(?:\s|>)/);
    expect(source.match(/<SelectGroup>/g)?.length).toBe(2);
    expect(source).toContain('aria-label={labels.model}');
    expect(source).toContain('aria-label={labels.status}');

    expect(encodeRequestHistorySelectValue("model", "model:gpt-test")).toBe(
      "model:model:gpt-test",
    );
    expect(decodeRequestHistorySelectValue("model", "model:model:gpt-test")).toBe(
      "model:gpt-test",
    );
    expect(decodeRequestHistorySelectValue("model", "model:")).toBe("");
    expect(decodeRequestHistorySelectValue("model", "status:failed")).toBe(
      "status:failed",
    );
  });

  it("详情中的 endpoint 会去掉查询参数和 hash，避免泄露 URL secret", () => {
    expect(formatSafeRequestEndpoint(
      "https://api.example.com/v1/responses?api_key=secret#top",
      "未报告",
    )).toBe("https://api.example.com/v1/responses");
    expect(formatSafeRequestEndpoint("relay.internal/path?token=secret", "未报告")).toBe(
      "relay.internal/path",
    );
    expect(formatSafeRequestEndpoint(null, "未报告")).toBe("未报告");
  });

  it("Provider 未报告 usage 时不会把 Token 伪装成明确的 0", () => {
    expect(formatRequestHistoryTokens({
      usageReported: false,
      inputTokens: 0,
      outputTokens: 0,
      estimated: false,
    }, "未报告")).toBe("未报告");
    expect(formatRequestHistoryTokens({
      usageReported: true,
      inputTokens: 0,
      outputTokens: 0,
      estimated: false,
    }, "未报告")).toBe("0");
  });

  it("详情弹窗投影所选请求的全部核心字段与错误信息", () => {
    const labels = new Proxy({} as RequestHistoryLabels, {
      get: (_target, key) => String(key),
    });
    const record: RequestRecord = {
      id: "request-2",
      logicalRequestId: "logical-2",
      attempt: 2,
      maxAttempts: 3,
      sessionId: "session-2",
      turnId: "turn-2",
      agentId: "agent-2",
      purpose: "completion",
      model: "model-selected",
      provider: "provider-selected",
      protocol: "responses",
      endpoint: "https://api.example.com/v1/responses?key=secret",
      requestMode: "stream",
      status: "failed",
      httpStatus: 500,
      errorKind: "http_status",
      error: "provider failed",
      requestedAtMs: new Date(2026, 7, 19, 10, 0, 0).getTime(),
      firstResponseAtMs: new Date(2026, 7, 19, 10, 0, 1).getTime(),
      completedAtMs: new Date(2026, 7, 19, 10, 0, 2).getTime(),
      durationMs: 2_000,
      usageReported: true,
      inputTokens: 120,
      outputTokens: 30,
      cacheCreationTokens: 10,
      cacheReadTokens: 20,
      estimated: false,
      providerRequestId: "provider-request-2",
    };

    const rows = Object.fromEntries(
      buildRequestHistoryDetailRows(record, "zh", labels).map((row) => [
        row.label,
        row,
      ]),
    );

    expect(rows.model?.value).toBe("model-selected");
    expect(rows.provider?.value).toBe("provider-selected");
    expect(rows.requestMode?.value).toBe("stream");
    expect(rows.attempt?.value).toBe("2/3");
    expect(rows.endpoint?.value).toBe("https://api.example.com/v1/responses");
    expect(rows.logicalRequestId?.value).toBe("logical-2");
    expect(rows.errorKind).toEqual({
      label: "errorKind",
      value: "statusHttp",
      error: true,
    });
    expect(rows.errorDetail).toEqual({
      label: "errorDetail",
      value: "provider failed",
      error: true,
    });
  });

  it("详情入口只打开共享弹窗，不再在表格行内展开", () => {
    const source = readFileSync(
      new URL("./RequestHistoryPanel.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain('aria-haspopup="dialog"');
    expect(source).toContain("setSelectedRecord(record)");
    expect(source).toContain("<GlassModal");
    expect(source).toContain("onClose={closeDetails}");
    expect(source).not.toContain("<details");
    expect(source).not.toContain("<summary");
  });
});
