import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { Card } from "./card";
import { Input } from "./input";
import { Select, SelectTrigger, SelectValue } from "./select";
import { Switch } from "./switch";
import { Tabs, TabsList, TabsTrigger } from "./tabs";
import { Textarea } from "./textarea";

const cardSource = readFileSync(new URL("./card.tsx", import.meta.url), "utf8");
const dialogSource = readFileSync(new URL("./dialog.tsx", import.meta.url), "utf8");
const dropdownSource = readFileSync(new URL("./dropdown-menu.tsx", import.meta.url), "utf8");
const selectSource = readFileSync(new URL("./select.tsx", import.meta.url), "utf8");
const tooltipSource = readFileSync(new URL("./tooltip.tsx", import.meta.url), "utf8");

describe("KeenCode shared control contract", () => {
  it("delegates input geometry to the official Appica md size", () => {
    const html = renderToStaticMarkup(<Input aria-label="搜索" />);

    expect(html).toContain('data-slot="input"');
    // 官方 inputVariants md：h-10 px-3.5 rounded-md（14px 根字号下约 35px 高）。
    expect(html).toContain("h-10");
    expect(html).not.toContain("keencode-control-md");
    // 项目输入表面色与焦点样式仍由 wrapper 锁定。
    expect(html).toContain("border-input-border");
    expect(html).toContain("focus-visible:ring-0");
    expect(html).toContain("text-ui-base");
  });

  it("delegates select and tab geometry to the official Appica md size", () => {
    const selectHtml = renderToStaticMarkup(
      <Select defaultValue="one">
        <SelectTrigger aria-label="选项">
          <SelectValue />
        </SelectTrigger>
      </Select>,
    );
    const tabsHtml = renderToStaticMarkup(
      <Tabs defaultValue="one">
        <TabsList>
          <TabsTrigger value="one">一</TabsTrigger>
        </TabsList>
      </Tabs>,
    );

    expect(selectHtml).toContain('data-slot="select-trigger"');
    expect(selectHtml).toContain("h-10");
    expect(selectHtml).not.toContain("keencode-control-md");
    expect(tabsHtml).toContain('data-slot="tabs-list"');
    expect(tabsHtml).not.toContain("keencode-tabs-list-md");
    expect(tabsHtml).toContain('data-slot="tabs-trigger"');
    expect(tabsHtml).not.toContain("h-[calc(100%-1px)]");
    // wrapper 保留的布局与字号类。
    expect(tabsHtml).toContain("flex-1");
    expect(tabsHtml).toContain("text-ui-base");
  });

  it("delegates textarea geometry to the official Appica md size", () => {
    const html = renderToStaticMarkup(<Textarea aria-label="说明" rows={2} />);

    expect(html).toContain('data-slot="textarea"');
    expect(html).not.toContain("keencode-textarea");
    expect(html).not.toContain("min-h-16");
    expect(html).toContain("text-ui-base");
  });

  it("keeps the shared card surface without overriding official spacing", () => {
    const cardHtml = renderToStaticMarkup(<Card>内容</Card>);

    expect(cardHtml).toContain('data-slot="card"');
    expect(cardHtml).toContain("bg-card");
    expect(cardHtml).not.toContain("data-inset");
    expect(cardHtml).not.toContain("data-size");
    expect(cardHtml).toContain('data-frame="none"');
    expect(cardSource).toContain("inset = false");
    expect(cardSource).toContain("frame = false");
    expect(cardSource).toContain('className={cn("text-foreground", className)}');
    // 内容层与插槽间距交给官方 Card，不再由 wrapper 覆盖。
    expect(cardSource).not.toContain("gap-4 py-4");
    expect(cardSource).not.toContain("gap-3 py-3");
  });

  it("keeps only surface colors and layout guards on popups", () => {
    expect(selectSource).toContain('"relative z-[60] border-border bg-popover text-foreground shadow-md [app-region:no-drag]"');
    expect(selectSource).not.toContain("min-h-7 px-2 py-1");
    expect(dropdownSource).toContain('"z-[60] max-w-(--available-width) min-w-32 border-border bg-popover text-foreground shadow-md [app-region:no-drag]"');
    expect(dropdownSource).toContain("text-ui-base/relaxed");
    // 弹层几何（max-h、内边距、条目尺寸）由官方提供。
    expect(dropdownSource).not.toContain("max-h-(--available-height)");
    expect(dropdownSource).not.toContain("*:p-1");
    expect(dropdownSource).not.toContain("min-h-7");
  });

  it("keeps dialog and tooltip semantic defaults without local geometry", () => {
    expect(dialogSource).toContain("frame = false");
    expect(dialogSource).toContain("border-border [&>[data-slot=dialog-content]]:overflow-hidden");
    expect(dialogSource).not.toContain('"gap-1 p-6"');
    expect(dialogSource).not.toContain('"min-h-0 flex-1 px-6"');
    expect(dialogSource).not.toContain('"gap-2 p-6"');
    expect(tooltipSource).toContain("delay = 0");
    expect(tooltipSource).toContain("delayMs = 0");
    expect(tooltipSource).not.toContain("px-3 py-1.5");
    expect(tooltipSource).toContain("bg-tooltip");
  });

  it("delegates switch geometry to the official Appica md size", () => {
    const html = renderToStaticMarkup(<Switch aria-label="启用" defaultChecked />);

    expect(html).toContain('data-slot="switch"');
    expect(html).toContain('data-slot="switch-thumb"');
    expect(html).not.toContain("keencode-switch");
    expect(html).not.toContain("h-[18px] w-8");
    expect(html).toContain('data-checked=""');
    // 官方 md 轨道几何。
    expect(html).toContain("h-5");
    expect(html).toContain("w-9.5");
    expect(html).toContain("peer");
  });
});
