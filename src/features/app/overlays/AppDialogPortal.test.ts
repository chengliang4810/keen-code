import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

describe("AppDialogPortal accessibility wiring", () => {
  it("traps keyboard focus and restores the opener when the dialog closes", () => {
    const source = readFileSync(
      new URL("./AppDialogPortal.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain('import { trapTabKey } from "@/lib/a11yFocus";');
    expect(source).toContain("ref={dialogRef}");
    expect(source).toContain(
      'document.addEventListener("keydown", onKeyDown, true)',
    );
    expect(source).toContain("previous?.isConnected");
    expect(source).toContain("previous.focus()");
  });

  it("为 prompt 输入框使用当前弹窗标题作为可访问名称", () => {
    const source = readFileSync(
      new URL("./AppDialogPortal.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain("aria-label={appDialog.title}");
  });
});
