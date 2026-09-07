import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./ProvidersPanel.tsx", import.meta.url), "utf8");
/** 仅锁定布局结构，颜色与控件主题继续由当前设计令牌管理。 */
const styles = readFileSync(new URL("../styles/app-resource.css", import.meta.url), "utf8");

describe("ProvidersPanel 历史双栏布局", () => {
  it("宽窗口保留左列表右表单，窄窗口按既有断点堆叠", () => {
    const split = styles.match(/\.prov-split\s*\{([^}]+)\}/)?.[1] ?? "";
    expect(split).toContain("grid-template-columns: minmax(220px, 280px) minmax(0, 1fr)");
    expect(split).toContain("min-height: 420px");
    expect(styles).toMatch(/@media \(max-width: 860px\)\s*\{\s*\.prov-split\s*\{\s*grid-template-columns: 1fr;\s*min-height: 0;/);
    expect(source.indexOf('className="prov-split__list"')).toBeLessThan(source.indexOf('className="prov-split__detail"'));
    expect(styles).toMatch(/\.prov-detail\s*\{[^}]*padding: 16px 18px;/);
  });
});
const settingsSource = readFileSync(new URL("./SettingsPage.tsx", import.meta.url), "utf8");

describe("ProvidersPanel 添加模型弹窗", () => {
  it("拉取后提供弹窗入口，表单不再内嵌在供应商详情", () => {
    const toolbar = source.slice(source.indexOf('className="prov-model-actions"'), source.indexOf('className="prov-model-list"'));
    expect(toolbar.indexOf("fetchModels()")).toBeLessThan(toolbar.indexOf("onClick={openAddModel}"));
    expect(toolbar).not.toContain("<Input");
    expect(source).toContain("open={modelAddOpen}");
    expect(source).toContain('form="provider-add-model-form"');
    expect(source).toContain("disabled={busy || !form.modelDraft.trim()}");
    expect(source).not.toContain('className="prov-model-add"');
  });

  it("复用现有提交逻辑及模态框的键盘与焦点行为", () => {
    const modal = source.slice(source.indexOf("open={modelAddOpen}"), source.indexOf("open={modelPickerOpen}"));
    expect(modal).toContain("data-modal-autofocus");
    expect(modal).toContain("addDraftModel()");
    for (const field of ["modelDraft", "contextWindowDraft", "context1mDraft", "supportsVisionDraft"]) {
      expect(modal).toContain(`form.${field}`);
    }
    expect(modal).not.toContain("providersUpsert");
    expect(modal).toContain("onClick={() => setModelAddOpen(false)}");
  });
});

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
