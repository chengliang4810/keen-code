import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./ProvidersPanel.tsx", import.meta.url), "utf8");
const settingsSource = readFileSync(new URL("./SettingsPage.tsx", import.meta.url), "utf8");

describe("ProvidersPanel 消息格式 Select 契约", () => {
  it("与设置页语言切换共享 shadcn Select，而不是旧的自定义下拉", () => {
    expect(source).toContain('from "@/components/ui/select"');
    expect(settingsSource).toContain('from "@/components/ui/select"');
    expect(source).not.toContain('from "@/components/Select"');
    expect(source).toContain("onValueChange={(value) =>");
    expect(source).toContain("<SelectContent>");
    expect(source).toContain("<SelectGroup>");
    expect(source).toContain("<SelectItem key={option.value} value={option.value}>");
    expect(source).toContain('className="settings-input"');
  });
});
