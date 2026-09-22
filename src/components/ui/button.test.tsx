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

function normalizedPath(file: string): string {
  return file.replaceAll("\\", "/");
}

describe("Button size contract", () => {
  it("uses the ZCode 28px baseline for standard actions", () => {
    const html = renderToStaticMarkup(<Button>保存</Button>);

    expect(html).toContain("h-7");
    expect(html).toContain("text-ui-base");
    expect(html).not.toContain("font-medium");
    expect(html).not.toContain("focus-visible:ring-2 focus-visible:ring-ring/30");
  });

  it("uses the matching 28px size for icon-only controls", () => {
    const html = renderToStaticMarkup(<Button size="icon-md" aria-label="关闭" />);

    expect(html).toContain("size-7");
    expect(html).toContain("rounded-lg");
  });

  it("keeps soft and ghost hover states on the shared hover token", () => {
    const soft = renderToStaticMarkup(<Button variant="soft">软按钮</Button>);
    const ghost = renderToStaticMarkup(<Button variant="ghost">幽灵按钮</Button>);

    expect(soft).toContain("hover:bg-hover");
    expect(ghost).toContain("hover:bg-hover");
  });

  it("routes business buttons through the shared size baseline", () => {
    const sourceRoot = new URL("../../", import.meta.url);
    const files = tsxFiles(sourceRoot).filter(
      (file) => !normalizedPath(file).endsWith("/components/ui/button.tsx"),
    );

    for (const file of files) {
      const source = readFileSync(file, "utf8");
      expect(source, file).not.toMatch(/@appica\/ui-react\/button["']/);
    }
  });

  it("allows ZCode compact and large sizes where layout semantics require them", () => {
    const source = readFileSync(
      new URL("../../features/app/sidebar/ProjectTree.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain('size="icon-sm"');
  });

  it("模型选择器保持中号触发按钮与中号列表", () => {
    const source = readFileSync(
      new URL("../ProviderModelMenu.tsx", import.meta.url),
      "utf8",
    );

    expect(source).toContain('<DropdownMenu size="md"');
    expect(source).not.toContain('<DropdownMenu size="lg"');
    const triggerButton = source.match(/<Button\b[\s\S]*?>/)?.[0];
    expect(triggerButton).toBeDefined();
    expect(triggerButton).not.toContain('size="lg"');
  });
});
