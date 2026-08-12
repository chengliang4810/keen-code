import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  loadResourceOpenTarget,
  loadResourceTreeWidth,
  RESOURCE_OPEN_TARGET_STORAGE_KEY,
  RESOURCE_TREE_WIDTH_STORAGE_KEY,
  saveResourceOpenTarget,
  saveResourceTreeWidth,
} from "@/lib/resourceViewerPreferences";

/** 创建资源栏偏好测试使用的内存存储。 */
function memoryPreferenceStorage(initial: Record<string, string> = {}) {
  const data = { ...initial };
  return {
    data,
    getItem(key: string) {
      return Object.prototype.hasOwnProperty.call(data, key) ? data[key]! : null;
    },
    setItem(key: string, value: string) {
      data[key] = value;
    },
  };
}

describe("ResourceViewer persistence", () => {
  it("仅在键缺失时使用资源栏首次启动值", () => {
    const storage = memoryPreferenceStorage();
    expect(loadResourceTreeWidth(storage)).toBe(220);
    expect(loadResourceOpenTarget(storage)).toBe("finder");

    storage.setItem(RESOURCE_TREE_WIDTH_STORAGE_KEY, "280");
    storage.setItem(RESOURCE_OPEN_TARGET_STORAGE_KEY, "system");
    expect(loadResourceTreeWidth(storage)).toBe(280);
    expect(loadResourceOpenTarget(storage)).toBe("system");
  });

  it("已存在的损坏值不会回退到默认值", () => {
    for (const value of ["", "139", "421", "220.5", " 220", "abc"]) {
      const storage = memoryPreferenceStorage({
        [RESOURCE_TREE_WIDTH_STORAGE_KEY]: value,
      });
      expect(() => loadResourceTreeWidth(storage)).toThrow();
    }
    const storage = memoryPreferenceStorage({
      [RESOURCE_OPEN_TARGET_STORAGE_KEY]: "Finder",
    });
    expect(() => loadResourceOpenTarget(storage)).toThrow("无效的打开目标");
  });

  it("校验后写入当前资源栏偏好", () => {
    const storage = memoryPreferenceStorage();
    saveResourceTreeWidth(320, storage);
    saveResourceOpenTarget("explorer", storage);
    expect(storage.data[RESOURCE_TREE_WIDTH_STORAGE_KEY]).toBe("320");
    expect(storage.data[RESOURCE_OPEN_TARGET_STORAGE_KEY]).toBe("explorer");
    expect(() => saveResourceTreeWidth(139, storage)).toThrow();
    expect(() => saveResourceOpenTarget("old-target" as never, storage)).toThrow();
  });

  it("存储读取和写入失败会直接传播", () => {
    const readFailure = new Error("read failed");
    expect(() =>
      loadResourceTreeWidth({
        getItem() {
          throw readFailure;
        },
      }),
    ).toThrow(readFailure);

    const writeFailure = new Error("write failed");
    expect(() =>
      saveResourceOpenTarget("system", {
        setItem() {
          throw writeFailure;
        },
      }),
    ).toThrow(writeFailure);
  });
});

describe("ResourceViewer controls", () => {
  it("文件和变更面板仅自动同步，不渲染手动刷新按钮", () => {
    const source = readFileSync(
      fileURLToPath(new URL("./ResourceViewer.tsx", import.meta.url)),
      "utf8",
    );

    expect(source).not.toContain("IconRefresh");
    expect(source).not.toContain('tr("resources.refresh")');
    expect(source).not.toContain('tr("changes.workspace.refresh")');
  });
});
