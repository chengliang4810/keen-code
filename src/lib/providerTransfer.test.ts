import { describe, expect, it } from "vitest";
import {
  checkProviderImportText,
  providerExportFilename,
  providerExportIds,
} from "./providerTransfer";

const doc = (providers: unknown) =>
  JSON.stringify({ schema: "keencode/providers-export", version: 1, providers });

describe("providerExportIds", () => {
  it("提取导出文档中的供应商标识", () => {
    expect(providerExportIds(doc([{ id: "a" }, { id: "b" }]))).toEqual([
      "a",
      "b",
    ]);
  });

  it("非 JSON 或缺少 providers 时返回空数组", () => {
    expect(providerExportIds("not-json")).toEqual([]);
    expect(providerExportIds(JSON.stringify({ schema: "x" }))).toEqual([]);
    expect(
      providerExportIds(doc([{ name: "no-id" }])),
    ).toEqual([]);
  });
});

describe("providerExportFilename", () => {
  it("使用供应商名称生成安全文件名", () => {
    expect(providerExportFilename("Work Relay!")).toBe(
      "keencode-providers-work-relay.json",
    );
  });

  it("中文名称原样保留，空名称回退固定前缀", () => {
    expect(providerExportFilename("中转服务")).toBe(
      "keencode-providers-中转服务.json",
    );
    expect(providerExportFilename("  ")).toBe(
      "keencode-providers-all.json",
    );
    expect(providerExportFilename(null)).toBe(
      "keencode-providers-all.json",
    );
  });
});

describe("checkProviderImportText", () => {
  it("接受包含供应商的导出文档", () => {
    const result = checkProviderImportText(doc([{ id: "a" }]));
    expect(result).toEqual({ ok: true, count: 1 });
  });

  it("拒绝非 JSON、未知 schema 和空列表", () => {
    expect(checkProviderImportText("nope")).toEqual({ ok: false, error: "json" });
    expect(
      checkProviderImportText(JSON.stringify({ schema: "other", providers: [{ id: "a" }] })),
    ).toEqual({ ok: false, error: "schema" });
    expect(checkProviderImportText(doc([]))).toEqual({
      ok: false,
      error: "empty",
    });
  });

  it("拒绝空文本", () => {
    expect(checkProviderImportText("   ")).toEqual({ ok: false, error: "json" });
  });
});
