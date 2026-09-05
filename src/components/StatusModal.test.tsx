import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it, vi } from "vitest";

/** 模拟打开与定位日志文件的 Tauri API。 */
const apiMocks = vi.hoisted(() => ({
  pathOpen: vi.fn(),
  pathReveal: vi.fn(),
}));
/** 模拟读取后端诊断日志路径的 ACP API。 */
const acpApiMocks = vi.hoisted(() => ({
  diagnosticsLogPath: vi.fn(),
}));

vi.mock("@/lib/api", () => apiMocks);
vi.mock("@/lib/acp/api", () => acpApiMocks);

import {
  performStatusModalDiagnosticsAction,
} from "./StatusModal";

describe("StatusModal 诊断日志操作", () => {
  afterEach(() => {
    apiMocks.pathOpen.mockReset();
    apiMocks.pathReveal.mockReset();
    acpApiMocks.diagnosticsLogPath.mockReset();
    vi.unstubAllGlobals();
  });

  it("把打开、定位和复制操作分别转发到对应系统 API", async () => {
    const path = "C:\\Users\\tester\\.keencode\\diagnostics.log";
    const writeText = vi.fn().mockResolvedValue(undefined);
    apiMocks.pathOpen.mockResolvedValue(undefined);
    apiMocks.pathReveal.mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });

    await performStatusModalDiagnosticsAction("open", path);
    await performStatusModalDiagnosticsAction("reveal", path);
    await performStatusModalDiagnosticsAction("copy", path);

    expect(apiMocks.pathOpen).toHaveBeenCalledWith(path);
    expect(apiMocks.pathReveal).toHaveBeenCalledWith(path);
    expect(writeText).toHaveBeenCalledWith(path);
  });

  it("保留系统操作异常，让组件负责在 Modal 内展示", async () => {
    const failure = new Error("path open failed");
    apiMocks.pathOpen.mockRejectedValue(failure);

    await expect(
      performStatusModalDiagnosticsAction("open", "C:\\diagnostics.log"),
    ).rejects.toBe(failure);
  });

  it("只在打开状态的 effect 中加载路径，并保持错误与按钮状态可见", () => {
    const source = readFileSync(
      new URL("./StatusModal.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("if (!open) {");
    expect(source).toContain("diagnosticsLogPath()");
    expect(source).toContain("}, [open]);");
    expect(source).toContain("const diagnosticsGeneration = useRef(0);");
    expect(source).toContain("diagnosticsActive.current");
    expect(source).not.toContain("setInterval");
    expect(source).toContain('role="alert"');
    expect(source).toContain('aria-live="polite"');
    expect(source).toContain("aria-busy={diagnosticsBusy}");
    expect(source).toContain("disabled={!diagnosticsPath || diagnosticsBusy}");
    expect(source).toContain('className="status-modal__diagnostics-actions"');
    expect(source).toContain('className="status-modal__log-path"');
    expect(source).not.toContain("ext-item__actions");
    expect(source).not.toContain("ext-alert");
    expect(source).toContain('runDiagnosticsAction("open")');
    expect(source).toContain('runDiagnosticsAction("reveal")');
    expect(source).toContain('runDiagnosticsAction("copy")');
  });
});
