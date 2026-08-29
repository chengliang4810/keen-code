import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const componentsRoot = new URL(".", import.meta.url);

function collectSourceFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return collectSourceFiles(path);
    if (!/\.(tsx|jsx)$/.test(entry.name) || /\.test\.(tsx|jsx)$/.test(entry.name)) {
      return [];
    }
    return [path];
  });
}

describe("选择控件统一契约", () => {
  const sources = collectSourceFiles(fileURLToPath(componentsRoot));
  const sourceText = sources
    .map((path) => readFileSync(path, "utf8"))
    .join("\n");

  it("组件源码不再渲染原生 select/option 下拉", () => {
    expect(sourceText).not.toMatch(/<select(?:\s|>)/);
    expect(sourceText).not.toMatch(/<option(?:\s|>)/);
    expect(sourceText).not.toMatch(/<optgroup(?:\s|>)/);
  });

  it("组件源码不再使用原生 radio 或自绘 radiogroup", () => {
    expect(sourceText).not.toMatch(/type=["']radio["']/);
    expect(sourceText).not.toMatch(/role=["']radiogroup["']/);
    expect(sourceText).not.toMatch(/role=["']radio["']/);
  });

  it("组件源码不再使用原生 checkbox", () => {
    expect(sourceText).not.toMatch(/type=["']checkbox["']/);
    const appSource = readFileSync(new URL("../App.tsx", componentsRoot), "utf8");
    expect(appSource).not.toMatch(/type=["']checkbox["']/);
  });

  it("统一选择原语来自项目的 shadcn/Radix 封装", () => {
    const settings = readFileSync(
      new URL("./SettingsPage.tsx", componentsRoot),
      "utf8",
    );
    const agents = readFileSync(
      new URL("./AgentsPanel.tsx", componentsRoot),
      "utf8",
    );

    expect(settings).toContain("@/components/ui/select");
    expect(settings).toContain("@/components/ui/toggle-group");
    expect(settings).toContain("@/components/ui/radio-group");
    expect(agents).toContain("@/components/ui/select");
    expect(agents).toContain("@/components/ui/radio-group");
    expect(agents).toContain("@/components/ui/checkbox");
  });
});
