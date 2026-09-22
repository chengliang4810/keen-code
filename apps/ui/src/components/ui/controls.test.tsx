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
const tooltipSource = readFileSync(new URL("./tooltip.tsx", import.meta.url), "utf8");

describe("KeenCode shared control contract", () => {
  it("keeps inputs on the ZCode 28px baseline", () => {
    const html = renderToStaticMarkup(<Input aria-label="搜索" />);

    expect(html).toContain('data-slot="input"');
    expect(html).toContain("keencode-control-md");
    expect(html).toContain("h-7");
    expect(html).toContain("rounded-md");
    expect(html).toContain("border-border");
    expect(html).toContain("focus-visible:ring-0");
    expect(html).toContain("text-ui-base");
  });

  it("keeps select and tab primitives on compact geometry while retaining Appica state", () => {
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
    expect(selectHtml).toContain("keencode-control-md");
    expect(selectHtml).toContain("h-7");
    expect(selectHtml).toContain("rounded-md");
    expect(selectHtml).toContain("focus-visible:ring-0");
    expect(selectHtml).toContain("text-ui-base/relaxed");
    expect(tabsHtml).toContain('data-slot="tabs-list"');
    expect(tabsHtml).toContain("keencode-tabs-list-md");
    expect(tabsHtml).toContain("h-8");
    expect(tabsHtml).toContain('data-slot="tabs-trigger"');
    expect(tabsHtml).toContain("h-[calc(100%-1px)]");
    expect(tabsHtml).toContain("flex-1");
    expect(tabsHtml).toContain("transition-all");
    expect(tabsHtml).toContain("focus-visible:outline-1");
    expect(tabsHtml).toContain("*:p-0");
  });

  it("routes textareas through the ZCode min-h-16 geometry wrapper", () => {
    const html = renderToStaticMarkup(<Textarea aria-label="说明" rows={2} />);

    expect(html).toContain('data-slot="textarea"');
    expect(html).toContain("keencode-textarea");
    expect(html).toContain("min-h-16");
    expect(html).toContain("px-2");
    expect(html).toContain("py-2");
    expect(html).toContain("text-ui-base");
  });

  it("uses the shared card surface and keeps menu items compact", () => {
    const cardHtml = renderToStaticMarkup(<Card size="sm">内容</Card>);
    const menuSource = readFileSync(new URL("./dropdown-menu.tsx", import.meta.url), "utf8");
    const selectSource = readFileSync(new URL("./select.tsx", import.meta.url), "utf8");

    expect(cardHtml).toContain('data-slot="card"');
    expect(cardHtml).toContain('data-size="sm"');
    expect(cardHtml).toContain("bg-card");
    expect(cardHtml).not.toContain("data-inset");
    expect(cardHtml).not.toContain("p-2");
    expect(cardHtml).toContain('data-frame="none"');
    expect(cardSource).toContain("inset = false");
    expect(cardSource).toContain("frame = false");
    expect(cardSource).toContain('className={cn("text-foreground", className)}');
    expect(menuSource).toContain('"z-[60] flex max-h-(--available-height)');
    expect(menuSource).toContain("gap-0.5 overflow-x-hidden overflow-y-auto");
    expect(menuSource).toContain("text-ui-base/relaxed");
    expect(selectSource).toContain('"relative z-[60] min-w-32 rounded-lg border border-border bg-popover');
  });

  it("collapses shared dialog, menu, and tooltip defaults to local geometry", () => {
    expect(dialogSource).toContain("frame = false");
    expect(dialogSource).toContain("rounded-2xl border border-border shadow-2xl");
    expect(dialogSource).toContain("[&>[data-slot=dialog-content]]:overflow-hidden");
    expect(dialogSource).toContain('"gap-1 p-6"');
    expect(dialogSource).toContain('"min-h-0 flex-1 px-6"');
    expect(dialogSource).toContain('"gap-2 p-6"');
    expect((dropdownSource.match(/max-h-\(--available-height\)[^\"]*\*:p-1/g) ?? []).length).toBe(2);
    expect(tooltipSource).toContain("delay = 0");
    expect(tooltipSource).toContain("delayMs = 0");
  });

  it("uses the ZCode 32x18 switch while delegating state to Appica", () => {
    const html = renderToStaticMarkup(<Switch aria-label="启用" defaultChecked />);

    expect(html).toContain('data-slot="switch"');
    expect(html).toContain('data-slot="switch-thumb"');
    expect(html).toContain('data-size="md"');
    expect(html).toContain("h-[18px] w-8");
    expect(html).toContain('data-checked=""');
    expect(html).toContain("border-transparent");
    expect(html).toContain("peer");
    expect(html).toContain("focus-visible:ring-2");
  });
});
