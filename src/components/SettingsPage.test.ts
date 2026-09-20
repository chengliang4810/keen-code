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
    expect(source).toContain('from "@appica/ui-react/select"');
    expect(languageSource).not.toMatch(/<select(?:\s|>)/);
    expect(languageSource.match(/<SelectGroup>/g)?.length).toBe(1);
    expect(languageSource).toContain("Object.entries(INTERFACE_LANGUAGE_LABELS)");
    expect(languageSource).toContain('typeof value === "string" && isLocale(value)');
    expect(languageSource).toContain("INTERFACE_LANGUAGE_LABELS[locale]");
    expect(source).toContain('zh: "中文简体"');
  });

  it("移动端设置导航使用分组 Select，并校验分区标识", () => {
    expect(source).not.toMatch(/<select(?:\s|>)/);
    expect(source).toContain("settings-page__mobile-select");
    expect(source).toContain("<SelectGroupLabel>");
    expect(source).toContain('typeof value === "string" && isSettingsSectionId(value)');
  });

  it("设置导航不再提供搜索入口", () => {
    expect(source).not.toContain("settings-page__search");
    expect(source).not.toContain("searchSettingsEntries");
  });
});

describe("SettingsPage 左侧导航排版契约", () => {
  it("主导航与返回入口使用和工作台主导航一致的 Appica 大号文字档位", () => {
    expect(source).toMatch(/className="settings-page__back"[\s\S]*?size="md"|size="md"[\s\S]*?className="settings-page__back"/);
    expect(source).toMatch(/<Navigation[\s\S]*?size="md"/);
  });

  it("返回应用的图标和文字从左侧起始线排列", () => {
    const styles = readFileSync(new URL("../styles/app-foundation.css", import.meta.url), "utf8");

    expect(styles).toMatch(
      /\.settings-page__back\s*\{[^}]*justify-content: flex-start;/,
    );
  });

  it("纵向导航列表、条目和链接占满侧栏可用宽度", () => {
    const styles = readFileSync(new URL("../styles/app-foundation.css", import.meta.url), "utf8");

    expect(styles).toMatch(
      /\.settings-page__nav-inner \[data-slot="navigation-list"\],[\s\S]*?\[data-slot="navigation-item"\],[\s\S]*?\[data-slot="navigation-link"\]\s*\{\s*width: 100%;/,
    );
  });
});

describe("SettingsPage 卡片间距契约", () => {
  it("主设置卡片和外观双栏卡片使用统一的桌面端内边距", () => {
    const styles = readFileSync(new URL("../styles/app-foundation.css", import.meta.url), "utf8");

    expect(styles).toMatch(
      /\.settings-page__main > \[data-slot="card"\] > \[data-slot="card-content"\],[\s\S]*?\.settings-appearance-duo > \[data-slot="card"\] > \[data-slot="card-content"\]\s*\{\s*padding: 16px;/,
    );
  });
});

describe("SettingsPage 主题选项排版", () => {
  it("主题按钮使用横向图标和文字", () => {
    const styles = readFileSync(new URL("../styles/app-foundation.css", import.meta.url), "utf8");

    expect(styles).toMatch(
      /\.ui-toggle-appearance\s*\{[\s\S]*?flex-direction:\s*row;[\s\S]*?gap:\s*8px;/,
    );
  });
});

describe("SettingsPage 控件尺寸契约", () => {
  it("设置页开关和数字输入统一使用中号尺寸", () => {
    expect(source).not.toContain('size="sm"');
    expect(source).toMatch(/<Switch[\s\S]*?size="md"/);
    expect(source).toMatch(/<NumberField[\s\S]*?size="md"/);
  });
});

describe("SettingsPage 托盘常驻契约", () => {
  it("关闭窗口后保留在系统托盘使用 Appica 开关，位于空闲睡眠开关之后", () => {
    const start = source.indexOf('id="settings-anchor-close-to-tray"');
    const end = source.indexOf(
      'id="settings-anchor-background-agent-limit"',
      start,
    );
    const traySource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(traySource).toContain("<SettingsSwitch");
    expect(traySource).not.toMatch(/<input(?:\s|>)/);
    expect(traySource).toContain('t("settings.closeToTray")');
    expect(traySource).toContain('t("settings.closeToTrayDesc")');
    expect(traySource).toContain("checked={closeToTray}");
    expect(traySource).toContain("onCloseToTray?.(checked)");
  });
});

describe("SettingsPage 后台任务并发契约", () => {
  it("后台 Agent 使用范围为 1 到 999 的数字输入", () => {
    const start = source.indexOf('id="settings-anchor-background-agent-limit"');
    const end = source.indexOf('id="settings-anchor-project-directory"', start);
    const limitSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(limitSource).toContain("<NumberField");
    expect(limitSource).toContain("min={MIN_BACKGROUND_AGENT_LIMIT}");
    expect(limitSource).toContain("max={MAX_BACKGROUND_AGENT_LIMIT}");
    expect(limitSource).toContain("onBackgroundAgentLimit(value)");
  });
});

describe("SettingsPage 兼容服务设置契约", () => {
  it("使用可聚焦的 Appica Input，失焦保存并允许清空恢复内置服务", () => {
    const start = source.indexOf('id="settings-anchor-web-service-url"');
    const end = source.indexOf(
      'id="settings-anchor-project-directory"',
      start,
    );
    const webServiceSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(webServiceSource).toContain("<label");
    expect(webServiceSource).toContain('htmlFor="settings-web-service-url"');
    expect(webServiceSource).toContain('<Input');
    expect(webServiceSource).toContain('id="settings-web-service-url"');
    expect(webServiceSource).toContain(
      'aria-describedby="settings-web-service-url-desc"',
    );
    expect(webServiceSource).toContain("const value = event.currentTarget.value.trim()");
    expect(webServiceSource).toContain("onWebServiceUrl(value)");
    expect(webServiceSource).toContain('event.key === "Enter"');
    expect(webServiceSource).not.toMatch(/<input(?:\s|>)/);
  });
});

describe("SettingsPage 界面字号契约", () => {
  it("界面字号使用 12–20 的 Appica 数字输入，失焦保存并校验范围", () => {
    const start = source.indexOf('id="settings-anchor-ui-font-size"');
    const end = source.indexOf('id="settings-anchor-terminal-font"', start);
    const fontSizeSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(fontSizeSource).toContain("<NumberField");
    expect(fontSizeSource).not.toMatch(/<input(?:\s|>)/);
    expect(fontSizeSource).toContain('id="settings-ui-font-size"');
    expect(fontSizeSource).toContain('htmlFor="settings-ui-font-size"');
    expect(fontSizeSource).toContain(
      'aria-describedby="settings-ui-font-size-desc"',
    );
    expect(fontSizeSource).toContain("min={MIN_UI_FONT_SIZE}");
    expect(fontSizeSource).toContain("max={MAX_UI_FONT_SIZE}");
    expect(fontSizeSource).toContain("if (value == null || !isUiFontSize(value)) return;");
    expect(fontSizeSource).toContain("onUiFontSize(value)");
    expect(fontSizeSource).toContain("onValueCommitted");
  });

  it("界面字号位于主题之后、终端设置之前", () => {
    const appearanceStart = source.indexOf('{section === "appearance" && (');
    const themeStart = source.indexOf('id="settings-anchor-theme"');
    const fontSizeStart = source.indexOf('id="settings-anchor-ui-font-size"');
    const terminalStart = source.indexOf('id="settings-anchor-terminal-font"');

    expect(appearanceStart).toBeGreaterThanOrEqual(0);
    expect(themeStart).toBeGreaterThan(appearanceStart);
    expect(fontSizeStart).toBeGreaterThan(themeStart);
    expect(terminalStart).toBeGreaterThan(fontSizeStart);
  });
});

describe("SettingsPage 主题和皮肤选择契约", () => {
  it("终端字体使用 Appica Input，并在失焦时保存非空字体族列表", () => {
    const start = source.indexOf('id="settings-anchor-terminal-font"');
    const end = source.indexOf('className="settings-appearance-duo"', start);
    const fontSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(fontSource).toContain("<Input");
    expect(fontSource).toContain("const value = event.currentTarget.value.trim()");
    expect(fontSource).toContain("onTerminalFontFamily(value)");
  });

  it("仅在 Windows 检测到 Shell 时展示集成终端选择", () => {
    expect(source).toContain("terminalShellOptions.length > 0");
    expect(source).toContain('<SelectItem value="auto">');
    expect(source).toContain("terminalShellOptions.map");
    expect(source).not.toContain('value="wsl"');
  });

  it("主题 segmented 使用受控 ToggleGroup，并由原语提供键盘导航", () => {
    const start = source.indexOf('id="settings-anchor-theme"');
    const end = source.indexOf('id="settings-anchor-skin"', start);
    const themeSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(source).toContain('from "@appica/ui-react/toggle-group"');
    expect(themeSource).toContain("<ToggleGroup");
    expect(themeSource).toContain("value={[themePreference]}");
    expect(themeSource).toContain(
      "if (isThemePreference(next)) onTheme(next)",
    );
    expect(themeSource.match(/<Toggle\b/g)?.length).toBe(3);
    expect(themeSource).not.toContain("onThemeKeyDown");
    expect(themeSource).not.toContain("data-theme-option");
    expect(themeSource).not.toContain('role="radiogroup"');
  });

  it("皮肤选择使用受控 ColorSwatchPicker，并校验持久化标识", () => {
    const start = source.indexOf('id="settings-anchor-skin"');
    const end = source.indexOf('id="settings-anchor-wallpaper"', start);
    const skinSource = source.slice(start, end);

    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(source).toContain('from "@appica/ui-react/color-swatch-picker"');
    expect(skinSource).toContain("<ColorSwatchPicker");
    expect(skinSource).toContain("<ColorSwatchPickerItem");
    expect(skinSource).toContain(
      "if (selected && isThemeSkinId(selected.id)) onSkin(selected.id)",
    );
  });
});
