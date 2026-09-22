import { describe, expect, it } from "vitest";
import {
  applySkinToDocument,
  applyWallpaperFlag,
  applyWallpaperBlurToDocument,
  applyWallpaperScrimToDocument,
  clearWallpaper,
  createIndexedDbWallpaperStorage,
  DEFAULT_SKIN,
  DEFAULT_WALLPAPER_BLUR,
  DEFAULT_WALLPAPER_SCRIM,
  getThemeSkinMeta,
  isThemeSkinId,
  loadSkin,
  loadWallpaperBlur,
  loadWallpaperRecord,
  loadWallpaperScrim,
  memoryWallpaperStorage,
  parseThemeSkin,
  parseWallpaperBlur,
  parseWallpaperScrim,
  prepareWallpaperFromFile,
  saveSkin,
  saveWallpaperBlur,
  saveWallpaper,
  saveWallpaperAdjust,
  saveWallpaperScrim,
  SKIN_STORAGE_KEY,
  skinPreferredTheme,
  THEME_SKINS,
  WALLPAPER_MAX_VIDEO_BYTES,
  WALLPAPER_BLUR_STORAGE_KEY,
  WALLPAPER_SCRIM_STORAGE_KEY,
  type WallpaperStorage,
  type SkinStorage,
  type WallpaperRecord,
} from "./themeSkin";

function memoryStorage(initial: Record<string, string> = {}): SkinStorage & {
  data: Record<string, string>;
  removeItem(key: string): void;
} {
  const data = { ...initial };
  return {
    data,
    getItem(key) {
      return key in data ? data[key]! : null;
    },
    setItem(key, value) {
      data[key] = value;
    },
    removeItem(key) {
      delete data[key];
    },
  };
}

/**
 * Duck-typed File. The video / gif / not_image branches only read `.type`,
 * `.name`, `.size` and pass the object through as the blob (no Blob methods
 * invoked), so a plain object suffices and lets us fake arbitrary sizes
 * without allocating megabytes.
 */
function fakeFile(type: string, name: string, size: number): File {
  return { type, name, size } as unknown as File;
}

/** 触发测试替身上已注册的 IndexedDB 事件处理器。 */
function dispatchIdbHandler(handler: unknown): void {
  if (typeof handler === "function") {
    (handler as () => void)();
  }
}

/** 创建会成功打开指定数据库替身的 IndexedDB 工厂。 */
function indexedDbFactoryFor(database: IDBDatabase): IDBFactory {
  return {
    open() {
      const request = {
        result: database,
        error: null,
        transaction: null,
        onupgradeneeded: null,
        onsuccess: null,
        onerror: null,
        onblocked: null,
      };
      queueMicrotask(() => dispatchIdbHandler(request.onsuccess));
      return request as unknown as IDBOpenDBRequest;
    },
  } as unknown as IDBFactory;
}

/** 创建对象仓库请求会失败的数据库替身。 */
function requestFailureDatabase(): IDBDatabase {
  return {
    objectStoreNames: { contains: () => true },
    close() {},
    transaction() {
      const transaction = {
        error: null,
        onerror: null,
        onabort: null,
        oncomplete: null,
        objectStore() {
          const fail = () => {
            const request = {
              result: undefined,
              error: new Error("request failed"),
              onsuccess: null,
              onerror: null,
            };
            queueMicrotask(() => dispatchIdbHandler(request.onerror));
            return request as unknown as IDBRequest<unknown>;
          };
          return {
            get: fail,
            put: fail,
            delete: fail,
          } as unknown as IDBObjectStore;
        },
      };
      return transaction as unknown as IDBTransaction;
    },
  } as unknown as IDBDatabase;
}

/** 返回一个可用于完整记录测试的壁纸对象。 */
function imageWallpaperRecord(): WallpaperRecord {
  return {
    kind: "image",
    mime: "image/jpeg",
    name: "p.jpg",
    createdAt: 1,
    blob: new Blob([new Uint8Array([1])], { type: "image/jpeg" }),
  };
}

describe("theme skins", () => {
  it("defaults to default and rejects unknown ids", () => {
    expect(DEFAULT_SKIN).toBe("default");
    expect(parseThemeSkin(null)).toBe("default");
    expect(() => parseThemeSkin("nope")).toThrow("主题外观格式无效");
    expect(() => parseThemeSkin("")).toThrow("主题外观格式无效");
    expect(() => parseThemeSkin(undefined)).toThrow("主题外观格式无效");
    expect(isThemeSkinId("gothic")).toBe(true);
    expect(isThemeSkinId("custom-x")).toBe(false);
  });

  it("ships stable preset ids inspired by Dream Skin packs", () => {
    const ids = THEME_SKINS.map((s) => s.id);
    expect(ids).toContain("default");
    expect(ids).toContain("rose");
    expect(ids).toContain("gothic");
    expect(ids).toContain("mist");
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("persists and reloads after simulated relaunch", () => {
    const storage = memoryStorage();
    expect(loadSkin(storage)).toBe("default");
    saveSkin(storage, "ocean");
    expect(storage.data[SKIN_STORAGE_KEY]).toBe("ocean");
    expect(loadSkin(storage)).toBe("ocean");
  });

  it("已存在的无效外观值会显式失败", () => {
    for (const raw of ["", "DEFAULT", "unknown"]) {
      const storage = memoryStorage({ [SKIN_STORAGE_KEY]: raw });
      expect(() => loadSkin(storage)).toThrow("主题外观格式无效");
    }
  });

  it("拒绝写入非当前外观值", () => {
    const storage = memoryStorage();
    expect(() => saveSkin(storage, "unknown" as never)).toThrow(
      "主题外观格式无效",
    );
    expect(storage.data).toEqual({});
  });

  it("no built-in skin forces dark/light (user shell stays put)", () => {
    for (const pack of THEME_SKINS) {
      expect(pack.appearance, pack.id).toBe("auto");
      expect(skinPreferredTheme(pack.id)).toBeNull();
    }
    expect(getThemeSkinMeta("gothic").appearance).toBe("auto");
  });

  it("applySkinToDocument sets or clears data-skin", () => {
    const attrs = new Map<string, string>();
    const el = {
      setAttribute(name: string, value: string) {
        attrs.set(name, value);
      },
      removeAttribute(name: string) {
        attrs.delete(name);
      },
    };
    applySkinToDocument("ember", el);
    expect(attrs.get("data-skin")).toBe("ember");
    applySkinToDocument("default", el);
    expect(attrs.has("data-skin")).toBe(false);
  });
});

describe("wallpaper storage", () => {
  it("IndexedDB 不可用或事务无法创建时显式失败", async () => {
    const unavailable = createIndexedDbWallpaperStorage(null);
    await expect(unavailable.get()).rejects.toThrow("壁纸存储打开失败");

    const database = {
      objectStoreNames: { contains: () => true },
      close() {},
      transaction() {
        throw new Error("transaction failed");
      },
    } as unknown as IDBDatabase;
    const storage = createIndexedDbWallpaperStorage(
      indexedDbFactoryFor(database),
    );
    await expect(storage.get()).rejects.toThrow("壁纸存储读取失败");
  });

  it("IndexedDB 读取、写入、更新和删除请求失败均显式拒绝", async () => {
    const storage = createIndexedDbWallpaperStorage(
      indexedDbFactoryFor(requestFailureDatabase()),
    );
    const record = imageWallpaperRecord();
    await expect(storage.get()).rejects.toThrow("壁纸存储读取失败");
    await expect(storage.set(record)).rejects.toThrow("壁纸存储写入失败");
    await expect(storage.update(() => record)).rejects.toThrow(
      "壁纸存储更新失败",
    );
    await expect(storage.clear()).rejects.toThrow("壁纸存储删除失败");
  });

  it("空事实源返回 null，非当前完整记录会显式失败", async () => {
    const storage = memoryWallpaperStorage();
    expect(await loadWallpaperRecord({ storage })).toBeNull();
    const blob = new Blob([new Uint8Array([1])], { type: "image/jpeg" });
    for (const value of [
      {
        kind: "image",
        mime: "image/jpeg",
        name: "p.jpg",
        createdAt: 1,
        unsupportedPath: "/tmp/p.jpg",
        blob,
      },
      {
        kind: "image",
        mime: "image/jpeg",
        name: "p.jpg",
        createdAt: 1,
        width: 1920,
        blob,
      },
      {
        kind: "image",
        mime: "image/jpeg",
        name: "p.jpg",
        createdAt: 1,
        focus: { cx: 2, cy: 0.5, zoom: 1 },
        blob,
      },
      {
        kind: "video",
        mime: "video/mp4",
        name: "v.mp4",
        createdAt: 1,
        clip: { start: 2, end: 1 },
        blob: new Blob([new Uint8Array([2])], { type: "video/mp4" }),
      },
      {
        kind: "image",
        mime: "image/jpeg",
        name: "p.jpg",
        createdAt: 1,
        clip: { start: 0, end: 2 },
        blob,
      },
      {
        kind: "video",
        mime: "image/jpeg",
        name: "v.mp4",
        createdAt: 1,
        blob,
      },
      {
        kind: "image",
        mime: "image/png",
        name: "p.png",
        createdAt: 1,
        blob,
      },
    ]) {
      await expect(storage.set(value as WallpaperRecord)).rejects.toThrow();
    }
  });

  it("round-trips a record through save / load / clear", async () => {
    const storage = memoryWallpaperStorage();
    const blob = new Blob([new Uint8Array([1, 2, 3, 4])], {
      type: "image/jpeg",
    });
    const record: WallpaperRecord = {
      kind: "image",
      mime: "image/jpeg",
      name: "p.jpg",
      createdAt: 1234567890,
      blob,
    };
    await saveWallpaper(record, { storage });

    const loaded = await loadWallpaperRecord({ storage });
    expect(loaded?.kind).toBe("image");
    expect(loaded?.mime).toBe("image/jpeg");
    expect(loaded?.name).toBe("p.jpg");
    expect(loaded?.createdAt).toBe(1234567890);
    expect(loaded?.blob).toBe(blob);

    await clearWallpaper({ storage });
    expect(await loadWallpaperRecord({ storage })).toBeNull();
  });

  it("完整记录只有一个事实源，删除后不保留独立元数据或 Blob", async () => {
    const storage = memoryWallpaperStorage();
    const blob = new Blob([new Uint8Array([7, 7, 7])], { type: "image/jpeg" });
    await saveWallpaper(
      {
        kind: "image",
        mime: "image/jpeg",
        name: "gone.jpg",
        createdAt: 42,
        blob,
      },
      { storage },
    );
    expect(storage._record?.blob).toBe(blob);
    await clearWallpaper({ storage });
    expect(storage._record).toBeNull();
    expect(await storage.get()).toBeNull();
  });

  it("读取、写入、更新和删除失败都向调用方传播", async () => {
    const failure = new Error("storage failed");
    const storage: WallpaperStorage = {
      get: async () => Promise.reject(failure),
      set: async () => Promise.reject(failure),
      update: async () => Promise.reject(failure),
      clear: async () => Promise.reject(failure),
    };
    const record: WallpaperRecord = {
      kind: "image",
      mime: "image/jpeg",
      name: "p.jpg",
      createdAt: 1,
      blob: new Blob([new Uint8Array([1])], { type: "image/jpeg" }),
    };
    await expect(loadWallpaperRecord({ storage })).rejects.toBe(failure);
    await expect(saveWallpaper(record, { storage })).rejects.toBe(failure);
    await expect(
      saveWallpaperAdjust({ focus: { cx: 0.5, cy: 0.5, zoom: 1 } }, { storage }),
    ).rejects.toBe(failure);
    await expect(clearWallpaper({ storage })).rejects.toBe(failure);
  });

  it("applyWallpaperFlag toggles the data-wallpaper attribute", () => {
    const attrs = new Map<string, string>();
    const el = {
      setAttribute(name: string, value: string) {
        attrs.set(name, value);
      },
      removeAttribute(name: string) {
        attrs.delete(name);
      },
    };
    applyWallpaperFlag(true, el);
    expect(attrs.get("data-wallpaper")).toBe("1");
    applyWallpaperFlag(false, el);
    expect(attrs.has("data-wallpaper")).toBe(false);
  });

  it("saveWallpaperAdjust 在同一完整记录中更新视频片段", async () => {
    const storage = memoryWallpaperStorage();
    const blob = new Blob([new Uint8Array([9, 9, 9])], { type: "video/mp4" });
    await saveWallpaper(
      {
        kind: "video",
        mime: "video/mp4",
        name: "v.mp4",
        createdAt: 1,
        blob,
      },
      { storage },
    );
    const saved = await saveWallpaperAdjust(
      {
        focus: { cx: 0.5, cy: 0.5, zoom: 1 },
        clip: { start: 1.2, end: 5.8 },
        duration: 12,
      },
      { storage },
    );
    expect(saved?.clip).toEqual({ start: 1.2, end: 5.8 });
    expect((await storage.get())?.clip).toEqual({ start: 1.2, end: 5.8 });
    expect((await storage.get())?.blob).toBe(blob);

    await saveWallpaperAdjust(
      { clip: { start: 0, end: 12 }, duration: 12 },
      { storage },
    );
    expect((await storage.get())?.clip).toBeUndefined();
  });
});

describe("wallpaper scrim", () => {
  it("仅缺失值使用默认强度，已存在的非当前格式会失败", () => {
    expect(DEFAULT_WALLPAPER_SCRIM).toBe(40);
    expect(parseWallpaperScrim(null)).toBe(40);
    expect(parseWallpaperScrim("0")).toBe(0);
    expect(parseWallpaperScrim("36")).toBe(36);
    expect(parseWallpaperScrim("100")).toBe(100);
    for (const raw of ["", " 36", "036", "35.6", "101", "nope", 36]) {
      expect(() => parseWallpaperScrim(raw)).toThrow(
        "壁纸遮罩强度格式无效",
      );
    }
  });

  it("persists and reloads scrim strength", () => {
    const storage = memoryStorage();
    expect(loadWallpaperScrim(storage)).toBe(DEFAULT_WALLPAPER_SCRIM);
    saveWallpaperScrim(storage, 42);
    expect(storage.data[WALLPAPER_SCRIM_STORAGE_KEY]).toBe("42");
    expect(loadWallpaperScrim(storage)).toBe(42);
  });

  it("保留界面运行时的夹取能力，但不对已存储值容错", () => {
    const storage = memoryStorage();
    saveWallpaperScrim(storage, -20);
    expect(storage.data[WALLPAPER_SCRIM_STORAGE_KEY]).toBe("0");
    saveWallpaperScrim(storage, 140);
    expect(storage.data[WALLPAPER_SCRIM_STORAGE_KEY]).toBe("100");

    storage.setItem(WALLPAPER_SCRIM_STORAGE_KEY, "140");
    expect(() => loadWallpaperScrim(storage)).toThrow(
      "壁纸遮罩强度格式无效",
    );
  });

  it("applyWallpaperScrimToDocument sets opacity + derived mix tokens", () => {
    const props = new Map<string, string>();
    const attrs = new Map<string, string>();
    const el = {
      setAttribute(name: string, value: string) {
        attrs.set(name, value);
      },
      removeAttribute(name: string) {
        attrs.delete(name);
      },
      style: {
        setProperty(name: string, value: string) {
          props.set(name, value);
        },
        removeProperty(name: string) {
          props.delete(name);
        },
      },
    };
    applyWallpaperScrimToDocument(25, el);
    expect(attrs.has("data-wallpaper-clear")).toBe(false);
    expect(props.get("--wallpaper-scrim-opacity")).toBe("0.250");
    expect(props.get("--wallpaper-mix-main")).toBe("18%"); // 70 * 0.25
    expect(props.get("--wallpaper-mix-sidebar")).toBe("15%"); // 58 * 0.25
    expect(props.get("--wallpaper-mix-settings")).toBe("20%"); // 78 * 0.25
    expect(props.get("--wallpaper-sidebar-blur")).toBe("5.5px");

    applyWallpaperScrimToDocument(0, el);
    expect(attrs.get("data-wallpaper-clear")).toBe("1");
    expect(props.get("--wallpaper-scrim-opacity")).toBe("0.000");
    expect(props.get("--wallpaper-mix-main")).toBe("0%");
    expect(props.get("--wallpaper-mix-sidebar")).toBe("0%");
    expect(props.get("--wallpaper-mix-settings")).toBe("0%");
    expect(props.get("--wallpaper-sidebar-blur")).toBe("0.0px");

    applyWallpaperScrimToDocument(100, el);
    expect(attrs.has("data-wallpaper-clear")).toBe(false);
    expect(props.get("--wallpaper-scrim-opacity")).toBe("1.000");
    expect(props.get("--wallpaper-mix-main")).toBe("70%");
    expect(props.get("--wallpaper-mix-sidebar")).toBe("58%");
    expect(props.get("--wallpaper-mix-settings")).toBe("78%");
    expect(props.get("--wallpaper-sidebar-blur")).toBe("22.0px");
  });
});

describe("wallpaper blur", () => {
  it("persists, clamps and applies blur", () => {
    const storage = memoryStorage();
    expect(loadWallpaperBlur(storage)).toBe(DEFAULT_WALLPAPER_BLUR);
    expect(parseWallpaperBlur("24")).toBe(24);
    expect(() => parseWallpaperBlur("25")).toThrow("壁纸模糊强度格式无效");
    saveWallpaperBlur(storage, 30);
    expect(storage.data[WALLPAPER_BLUR_STORAGE_KEY]).toBe("24");
    const props = new Map<string, string>();
    applyWallpaperBlurToDocument(12, {
      setAttribute() {},
      removeAttribute() {},
      style: {
        setProperty: (name, value) => void props.set(name, value),
        removeProperty() {},
      },
    });
    expect(props.get("--wallpaper-blur")).toBe("12px");
    expect(props.get("--wallpaper-blur-scale")).toBe("1.050");
  });
});

describe("prepareWallpaperFromFile", () => {
  it("accepts a small mp4 as-is", async () => {
    const file = fakeFile("video/mp4", "clip.mp4", 1024);
    const rec = await prepareWallpaperFromFile(file);
    expect(rec.kind).toBe("video");
    expect(rec.mime).toBe("video/mp4");
    expect(rec.name).toBe("clip.mp4");
    expect(rec.blob).toBe(file);
  });

  it("rejects unsupported video mimetypes", async () => {
    const file = fakeFile("video/quicktime", "clip.mov", 1024);
    await expect(prepareWallpaperFromFile(file)).rejects.toMatchObject({
      code: "unsupported_video",
    });
  });

  it("rejects oversized video", async () => {
    const file = fakeFile(
      "video/mp4",
      "big.mp4",
      WALLPAPER_MAX_VIDEO_BYTES + 1,
    );
    await expect(prepareWallpaperFromFile(file)).rejects.toMatchObject({
      code: "video_too_large",
    });
  });

  it("preserves an animated gif as-is (no recompress)", async () => {
    const file = fakeFile("image/gif", "anim.gif", 2048);
    const rec = await prepareWallpaperFromFile(file);
    expect(rec.kind).toBe("image");
    expect(rec.mime).toBe("image/gif");
    expect(rec.blob).toBe(file);
  });

  it("rejects non-image / non-video files", async () => {
    const file = fakeFile("text/plain", "notes.txt", 8);
    await expect(prepareWallpaperFromFile(file)).rejects.toMatchObject({
      code: "not_image",
    });
  });
});
