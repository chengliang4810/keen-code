import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./SettingsPage.tsx", import.meta.url), "utf8");

describe("SettingsPage Select 契约", () => {
  it("界面语言使用分组 Select，而不是原生下拉", () => {
    const start = source.indexOf('id="settings-anchor-interface-language"');
    const end = source.indexOf(
      'id="settings-anchor-hardware-acceleration"',
      start,
    );
    const languageSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(source).toContain('from "@/components/ui/select"');
    expect(languageSource).not.toMatch(/<select(?:\s|>)/);
    expect(languageSource.match(/<SelectGroup>/g)?.length).toBe(1);
    expect(languageSource.match(/<SelectItem\b/g)?.length).toBe(3);
    expect(languageSource).toContain("if (isLocale(value)) onLocaleChange(value)");
  });

  it("移动端设置导航使用分组 Select，并校验分区标识", () => {
    expect(source).not.toMatch(/<select(?:\s|>)/);
    expect(source).toContain("settings-page__mobile-select");
    expect(source).toContain("<SelectLabel>");
    expect(source).toContain("if (isSettingsSectionId(value)) openSection(value)");
  });

  it("设置导航不再提供搜索入口", () => {
    expect(source).not.toContain("settings-page__search");
    expect(source).not.toContain("searchSettingsEntries");
  });
});

describe("SettingsPage 后台任务并发契约", () => {
  it("后台 Agent 使用范围为 1 到 999 的数字输入", () => {
    const start = source.indexOf('id="settings-anchor-background-agent-limit"');
    const end = source.indexOf('id="settings-anchor-project-directory"', start);
    const limitSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(limitSource).toContain('type="number"');
    expect(limitSource).toContain("min={MIN_BACKGROUND_AGENT_LIMIT}");
    expect(limitSource).toContain("max={MAX_BACKGROUND_AGENT_LIMIT}");
    expect(limitSource).toContain("onBackgroundAgentLimit(value)");
  });
});

describe("SettingsPage 主题和皮肤选择契约", () => {
  it("终端字体使用 shadcn Input，并在失焦时保存非空字体族列表", () => {
    const start = source.indexOf('id="settings-anchor-terminal-font"');
    const end = source.indexOf('className="settings-appearance-duo"', start);
    const fontSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(fontSource).toContain("<Input");
    expect(fontSource).toContain("const value = event.currentTarget.value.trim()");
    expect(fontSource).toContain("onTerminalFontFamily(value)");
  });

  it("主题 segmented 使用受控 ToggleGroup，并由原语提供键盘导航", () => {
    const start = source.indexOf('id="settings-anchor-theme"');
    const end = source.indexOf('id="settings-anchor-skin"', start);
    const themeSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(source).toContain('from "@/components/ui/toggle-group"');
    expect(themeSource).toContain("<ToggleGroup");
    expect(themeSource).toContain('type="single"');
    expect(themeSource).toContain("value={themePreference}");
    expect(themeSource).toContain(
      "if (isThemePreference(value)) onTheme(value)",
    );
    expect(themeSource.match(/<ToggleGroupItem\b/g)?.length).toBe(3);
    expect(themeSource).not.toContain("onThemeKeyDown");
    expect(themeSource).not.toContain("data-theme-option");
    expect(themeSource).not.toContain('role="radiogroup"');
  });

  it("皮肤卡片使用受控 RadioGroup，并校验持久化标识", () => {
    const start = source.indexOf('id="settings-anchor-skin"');
    const end = source.indexOf('id="settings-anchor-wallpaper"', start);
    const skinSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(source).toContain('from "@/components/ui/radio-group"');
    expect(skinSource).toContain("<RadioGroup");
    expect(skinSource).toContain("value={skin}");
    expect(skinSource).toContain(
      "if (isThemeSkinId(value)) onSkin(value)",
    );
    expect(skinSource).toContain("<RadioGroupItem");
    expect(skinSource).not.toContain('role="listbox"');
    expect(skinSource).not.toContain('role="option"');
    expect(skinSource).not.toContain('role="radiogroup"');
  });
});
