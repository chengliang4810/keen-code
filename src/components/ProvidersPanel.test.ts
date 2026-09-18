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

  it("供应商列表与右栏等高并在内部滚动，滚动条沿用设置页静默样式", () => {
    const list = styles.match(/\.prov-split__list\s*\{([^}]+)\}/)?.[1] ?? "";
    // size containment：列表内容不计入行高，列拉伸到与右侧表单等高，溢出在 rail 内滚动。
    expect(list).toContain("contain: size");
    // 窄屏堆叠布局恢复内容高度，保留 280px 上限，避免 containment 把列表塌成 0。
    expect(styles).toMatch(
      /\.prov-split__list\s*\{[^}]*contain: none;[^}]*max-height: 280px;/,
    );
    // 全局默认隐藏滚动条，列表加入设置页静默例外组，悬停或聚焦时出现细滚动条。
    expect(styles).toMatch(/\.prov-rail\s*\{[^}]*scrollbar-width: thin/);
    expect(styles).toMatch(
      /\.prov-rail:hover[^{]*\{[^}]*scrollbar-color: var\(--scrollbar-thumb\)/,
    );
    // rail 自身保持弹性滚动容器，高度约束来自栅格行的拉伸。
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
