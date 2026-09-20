import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Button } from "@/components/ui/button";

function tsxFiles(directory: URL): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const child = new URL(`${entry.name}${entry.isDirectory() ? "/" : ""}`, directory);
    if (entry.isDirectory()) return tsxFiles(child);
    return entry.name.endsWith(".tsx") ? [fileURLToPath(child)] : [];
  });
}

describe("Button size contract", () => {
  it("uses the medium Appica size for standard actions", () => {
    const html = renderToStaticMarkup(<Button>保存</Button>);

    expect(html).toContain("h-10");
    expect(html).toContain("text-sm");
  });

  it("uses the matching medium size for icon-only controls", () => {
    const html = renderToStaticMarkup(<Button size="icon-md" aria-label="关闭" />);

    expect(html).toContain("size-10");
  });

  it("routes business buttons through the shared size baseline", () => {
    const sourceRoot = new URL("../../", import.meta.url);
    const files = tsxFiles(sourceRoot).filter(
      (file) => !file.endsWith("/components/ui/button.tsx"),
    );

    for (const file of files) {
      const source = readFileSync(file, "utf8");
      expect(source, file).not.toMatch(/@appica\/ui-react\/button["']/);
      for (const [openingTag] of source.matchAll(/<Button\b[\s\S]*?>/g)) {
        expect(openingTag, file).not.toMatch(/\bsize="(?:icon-)?(?:sm|lg)"/);
      }
    }
  });

  it("keeps application controls on the medium size baseline", () => {
    const sourceRoot = new URL("../../", import.meta.url);
    const files = tsxFiles(sourceRoot).filter(
      (file) =>
        !file.includes(".test.") &&
        !file.endsWith("/components/ProviderModelMenu.tsx"),
    );

    for (const file of files) {
      const source = readFileSync(file, "utf8");
      expect(source, file).not.toMatch(/\bsize="(?:icon-)?(?:sm|lg)"/);
    }
  });

  it("模型选择器保持中号触发按钮，展开菜单使用大号列表", () => {
    const source = readFileSync(
      new URL("../ProviderModelMenu.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain('<DropdownMenu size="lg"');
    const triggerButton = source.match(/<Button\b[\s\S]*?>/)?.[0];
    expect(triggerButton).toBeDefined();
    expect(triggerButton).not.toContain('size="lg"');
  });
});
