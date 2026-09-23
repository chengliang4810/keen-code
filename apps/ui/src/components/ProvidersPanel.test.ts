import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./ProvidersPanel.tsx", import.meta.url), "utf8");
/** 仅锁定布局结构，颜色与控件主题继续由当前设计令牌管理。 */
const styles = readFileSync(new URL("../styles/app-resource.css", import.meta.url), "utf8");

describe("ProvidersPanel 双栏布局", () => {
  it("宽窗口使用稳定窄列表与自然高度表单，窄窗口按既有断点堆叠", () => {
    const split = styles.match(/\.prov-split\s*\{([^}]+)\}/)?.[1] ?? "";
    expect(split).toContain("grid-template-columns: minmax(15rem, 17rem) minmax(0, 1fr)");
    expect(split).toContain("align-items: start");
    expect(styles).toMatch(/@media \(max-width: 860px\)\s*\{\s*\.prov-split\s*\{\s*grid-template-columns: 1fr;\s*min-height: 0;/);
    expect(source.indexOf('className="prov-split__list"')).toBeLessThan(source.indexOf('className="prov-split__detail"'));
    expect(styles).toMatch(/\.prov-detail\s*\{[^}]*height: auto;[^}]*background: transparent;[^}]*box-shadow: none;/);
    // 表单卡内距由设置卡统一契约（settings-shell.css 内容层默认 16px）提供；
    // 清零例外清单只允许 extensions 面板卡，不得再收录 prov-detail。
    const shellStyles = readFileSync(new URL("../styles/settings-shell.css", import.meta.url), "utf8");
    expect(shellStyles).toMatch(/\.settings-page__body \[data-slot="card-content"\]\s*\{\s*padding: 16px;/);
    expect(shellStyles).not.toMatch(/\.prov-detail[^{]*\{\s*padding: 0;/);
    expect(source).not.toContain("prov-detail__content");
    expect(source).toContain("inset={false}");
  });

  it("供应商列表使用适合桌面的行高与独立滚动区域", () => {
    const list = styles.match(/\.prov-split__list\s*\{([^}]+)\}/)?.[1] ?? "";
    expect(list).toContain("max-height: min(38rem, calc(100vh - 10rem))");
    expect(styles).toMatch(/\.prov-item__main\s*\{[^}]*min-height: 3\.25rem;[^}]*justify-content: flex-start;/);
    expect(styles).toMatch(/@media \(max-width: 860px\)[\s\S]*?\.prov-split__list\s*\{[^}]*max-height: 280px;/);
    // rail 自身保持弹性滚动容器，高度约束来自列表列。
    expect(styles).toMatch(/\.prov-rail\s*\{[^}]*overflow: auto;/);
    // rail 子项禁止收缩：超高交给 rail 滚动，而不是把单项压缩塞满。
    expect(styles).toMatch(/\.prov-item\s*\{[^}]*flex-shrink: 0;/);
    expect(styles).toMatch(/\.prov-rail-empty\s*\{[^}]*flex-shrink: 0;/);
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
    expect(source).toContain("disabled={busy || loadingMetadata || !form.modelDraft.trim()}");
    expect(source).not.toContain('className="prov-model-add"');
  });

  it("复用现有提交逻辑及模态框的键盘与焦点行为", () => {
    const modal = source.slice(source.indexOf("open={modelAddOpen}"), source.indexOf("open={modelPickerOpen}"));
    expect(modal).toContain("data-modal-autofocus");
    expect(modal).toContain("addDraftModel()");
    for (const field of ["modelDraft", "contextWindowDraft", "supportsVisionDraft"]) {
      expect(modal).toContain(`form.${field}`);
    }
    expect(modal).not.toContain("providersUpsert");
    expect(modal).toContain("onClick={closeModelEditor}");
  });
});

describe("ProvidersPanel 消息格式 Select 契约", () => {
  it("与设置页语言切换共享 Appica Select，而不是旧的自定义下拉", () => {
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

describe("ProvidersPanel 供应商信息导出与导入", () => {
  it("编辑表单头部提供复制与导出入口，新增模式不显示", () => {
    const head = source.slice(
      source.indexOf('className="prov-form__head"'),
      source.indexOf('className="prov-form__grid"'),
    );
    expect(head).toContain("providers.find((item) => item.id === editingId)");
    expect(head).toContain("copyProvider(provider)");
    expect(head).toContain("exportProvider(provider)");
    expect(head).toContain("tr(\"prov.copy\")");
    expect(head).toContain("tr(\"prov.exportOne\")");
    // 两个入口只在已有供应商（编辑态）出现，新建草稿没有可导出的持久配置。
    expect(head).toContain("editingId ? (");
  });

  it("新增表单右上角提供导入入口，编辑态不显示；导入走后端合并而不是本地逐个 upsert", () => {
    const head = source.slice(
      source.indexOf('className="prov-form__head"'),
      source.indexOf('className="prov-form__grid"'),
    );
    expect(head).toContain('tr("prov.importAll")');
    // 导入入口只在新增分支（编辑态三元的 else 侧），编辑态右上角仅为复制与导出。
    expect(head.indexOf(") : (")).toBeGreaterThan(-1);
    expect(head.indexOf('tr("prov.importAll")')).toBeGreaterThan(
      head.indexOf(") : ("),
    );
    // 左栏不再提供导入，导出全部入口保持移除。
    const rail = source.slice(
      source.indexOf('className="prov-split__list"'),
      source.indexOf('className="prov-rail"'),
    );
    expect(rail).not.toContain("prov.importAll");
    expect(rail).not.toContain("prov.exportAll");
    expect(rail).not.toContain("exportAllProviders");
    const transfer = source.slice(
      source.indexOf("const submitImport"),
      source.indexOf("/** 切换远端模型的勾选状态"),
    );
    expect(transfer).toContain("api.providersImport(importDraft.text)");
    expect(transfer).not.toContain("providersUpsert");
  });

  it("导入弹窗在选择文件后先本地预检结构，再启用提交", () => {
    const modal = source.slice(source.indexOf("open={importDraft !== null}"));
    expect(modal).toContain("pickImportFile()");
    expect(modal).toContain("importDraft.check.ok");
    expect(modal).toContain("submitImport()");
    expect(modal).toContain("tr(\"prov.importWorking\")");
  });
});

describe("ProvidersPanel 表单字段布局", () => {
  it("API Key 字段独占表单整行", () => {
    const apiKeyAt = source.indexOf('tr("prov.apiKey")');
    const fieldAt = source.lastIndexOf('<Field className="prov-field--full">', apiKeyAt);
    expect(fieldAt).toBeGreaterThan(-1);
    expect(source.slice(fieldAt, apiKeyAt)).toContain("prov-field--full");
  });
});
