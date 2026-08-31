import { describe, expect, it } from "vitest";
import {
  countWorkspaceChangeFiles,
  mergeLoadedTree,
  replaceWorkspaceDirectory,
} from "@/lib/resourceViewerTree";
import {
  loadResourceOpenTarget,
  loadResourceTreeWidth,
  RESOURCE_OPEN_TARGET_STORAGE_KEY,
  RESOURCE_TREE_WIDTH_STORAGE_KEY,
  saveResourceOpenTarget,
  saveResourceTreeWidth,
} from "@/lib/resourceViewerPreferences";
import { readSource } from "../test-utils/readCssSource";

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
  it("静默刷新保留可见子树但使目录在下次展开时重新读取", () => {
    const previous = [
      {
        name: "src",
        relativePath: "src",
        isDir: true,
        size: 0,
        ext: "",
        loaded: true,
        children: [
          {
            name: "nested",
            relativePath: "src/nested",
            isDir: true,
            size: 0,
            ext: "",
            loaded: true,
            children: [],
          },
          {
            name: "old.ts",
            relativePath: "src/old.ts",
            isDir: false,
            size: 1,
            ext: "ts",
            loaded: true,
          },
        ],
      },
      {
        name: "deleted.txt",
        relativePath: "deleted.txt",
        isDir: false,
        size: 1,
        ext: "txt",
        loaded: true,
      },
    ];
    const refreshed = mergeLoadedTree(previous, [
      {
        name: "src",
        relativePath: "src",
        isDir: true,
        size: 0,
        ext: "",
        loaded: false,
        children: [],
      },
      {
        name: "added.txt",
        relativePath: "added.txt",
        isDir: false,
        size: 2,
        ext: "txt",
        loaded: true,
      },
    ]);

    expect(refreshed.map((entry) => entry.relativePath)).toEqual([
      "src",
      "added.txt",
    ]);
    expect(refreshed[0]!.children?.[1]?.relativePath).toBe("src/old.ts");
    expect(refreshed[0]!.loaded).toBe(false);
    expect(refreshed[0]!.children?.[0]?.loaded).toBe(false);
  });

  it("未跟踪目录占位按需替换为实际文件并维持非零计数", () => {
    const directory = {
      path: "vendor",
      absolutePath: "D:/repo/vendor",
      status: "??",
      indexStatus: "?",
      worktreeStatus: "?",
      kind: "untracked" as const,
      name: "vendor",
      isDirectory: true,
      isNestedRepository: false,
    };
    const files = ["vendor/a.ts", "vendor/b.ts"].map((path) => ({
      ...directory,
      path,
      absolutePath: `D:/repo/${path}`,
      name: path.split("/").at(-1)!,
      isDirectory: false,
    }));

    expect(countWorkspaceChangeFiles([directory])).toBe(1);
    const expanded = replaceWorkspaceDirectory([directory], "vendor", files);
    expect(expanded.map((entry) => entry.path)).toEqual([
      "vendor/a.ts",
      "vendor/b.ts",
    ]);
    expect(countWorkspaceChangeFiles(expanded)).toBe(2);
  });

  it("文件和变更面板仅自动同步，不渲染手动刷新按钮", () => {
    const source = readSource(new URL("./ResourceViewer.tsx", import.meta.url));

    expect(source).not.toContain("IconRefresh");
    expect(source).not.toContain('tr("resources.refresh")');
    expect(source).not.toContain('tr("changes.workspace.refresh")');
  });

  it("子智能体使用独立顶层标签并复用主对话渲染器", () => {
    const source = readSource(new URL("./ResourceViewer.tsx", import.meta.url));

    expect(source).toContain('type: "subagent"');
    expect(source).toContain('setSideMode("subagent")');
    expect(source).toContain("<ConversationThread");
  });

  it("打开面板时已有文件树和变更缓存则静默刷新", () => {
    const source = readSource(new URL("./ResourceViewer.tsx", import.meta.url));

    expect(source).toContain("mergeLoadedTree(prev, next)");
    expect(source).toContain("treeHasSnapshot");
    expect(source).toContain("workspaceHasSnapshot");
    expect(source).toMatch(/const showSpinner = !treeHasSnapshot\.current/);
    expect(source).toMatch(/const showSpinner = !workspaceHasSnapshot\.current/);
    expect(source).toContain("if (showSpinner) setLoadingTree(true)");
    expect(source).toContain("if (showSpinner) setWorkspaceLoading(true)");
    expect(source).toContain("snapshotProjectPath");
  });

  it("仅为当前可见的文件或变更模式执行对应刷新", () => {
    const source = readSource(new URL("./ResourceViewer.tsx", import.meta.url));

    expect(source).toContain(
      'if (!paneActive || sideMode !== "changes") return;',
    );
    expect(source).toContain(
      'if (!paneActive || sideMode !== "files" || !projectPath) return;',
    );
    expect(source).toMatch(
      /\[paneActive, refreshWorkspaceStatus, sideMode, syncRevision\]/,
    );
    expect(source).toMatch(
      /\[paneActive, projectPath, refresh, sideMode, syncRevision\]/,
    );
    expect(source).toContain("WORKSPACE_SYNC_DEBOUNCE_MS = 200");
    expect(source).toContain("workspaceSyncRevision.current === syncRevision");
    expect(source).toContain("void refreshWorkspaceStatus(true)");
    expect(source).toContain("api.gitStatus(projectPath, { force })");
    expect(source).toContain("TREE_SYNC_DEBOUNCE_MS = 200");
    expect(source).toContain("treeRefreshInFlight");
    expect(source).toContain("existing.queued = true");
  });

  it("目录变更只惰性读取子项，不进入文件 Diff 请求链", () => {
    const source = readSource(new URL("./ResourceViewer.tsx", import.meta.url));
    const diffLoader = source
      .split("const loadWorkspaceDiff")
      .at(1)!
      .split("const revealChangePath")[0]!;

    expect(source).toContain("api.gitUntrackedDirectory(projectPath, key)");
    expect(source).not.toContain("api.fsListDir(projectPath, key)");
    expect(diffLoader).toContain("if (entry.isDirectory)");
    expect(diffLoader.indexOf("if (entry.isDirectory)")).toBeLessThan(
      diffLoader.indexOf("api.gitFileDiff"),
    );
  });

  it("已选统一 Diff 在文件和变更模式切换时保持挂载", () => {
    const source = readSource(new URL("./ResourceViewer.tsx", import.meta.url));

    expect(source).toContain("rp-change-preview--persistent");
    expect(source).toContain("aria-hidden={!persistentDiffVisible}");
    expect(source).toMatch(
      /<StructuredDiffPreview\s+patch=\{diffView\.unified\}\s+locale=\{locale\}/,
    );
    expect(source).toContain("persistentDiffVisible ? null");
  });
});
