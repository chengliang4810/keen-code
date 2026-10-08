import { Select, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { SettingRow } from "@/settings/components/SettingRow";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

function fieldLabel(markup: string, role: string): string {
  const control = markup.match(new RegExp(`<[^>]+role="${role}"[^>]*>`))?.[0];
  expect(control).toBeDefined();
  const labelledBy = control?.match(/aria-labelledby="([^"]+)"/)?.[1];
  expect(labelledBy).toBeDefined();
  const span = markup.match(/<span id="([^"]+)"[^>]*>([^<]+)<\/span>/);
  expect(span?.[1]).toBe(labelledBy);
  return span?.[2] ?? "";
}

describe("settings control labels", () => {
  it.each(["自动保存", "Auto-save"])("names switches from the current visible title %s", (title) => {
    const markup = renderToStaticMarkup(<SettingRow title={title}><Switch checked /></SettingRow>);
    expect(fieldLabel(markup, "switch")).toBe(title);
    expect(markup).toContain('aria-checked="true"');
  });

  it("labels the select trigger across its Select wrapper", () => {
    const markup = renderToStaticMarkup(<SettingRow title="Font size"><Select><SelectTrigger><SelectValue /></SelectTrigger></Select></SettingRow>);
    expect(fieldLabel(markup, "combobox")).toBe("Font size");
  });

  it("labels the actual slider thumb", () => {
    const markup = renderToStaticMarkup(<SettingRow title="Zoom"><Slider value={[100]} min={50} max={200} /></SettingRow>);
    expect(fieldLabel(markup, "slider")).toBe("Zoom");
  });

  it("preserves an explicit accessible name", () => {
    const markup = renderToStaticMarkup(<SettingRow title="Notifications"><Switch aria-label="Notification sound" /></SettingRow>);
    const control = markup.match(/<[^>]+role="switch"[^>]*>/)?.[0];
    expect(control).toContain('aria-label="Notification sound"');
    expect(control).not.toContain("aria-labelledby");
  });
});
