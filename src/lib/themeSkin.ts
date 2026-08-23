/**
 * Color skins on top of dark/light (inspired by Codex Dream Skin presets).
 * Skins only remap design tokens via `data-skin` — they do not inject foreign CSS
 * or touch native chrome beyond optional preferred appearance.
 *
 * Optional custom wallpaper: user image / video / animated gif and its metadata
 * are persisted as one IndexedDB record. Rendered by a React media layer
 * (`<video>` / `<img>`) with absolute layout from {@link WallpaperFocus}
 * (pan + zoom). Source assets are never mutated / re-encoded.
 */

import type { Theme } from "./theme";
import {
  DEFAULT_WALLPAPER_FOCUS,
  normalizeWallpaperFocus,
  parseWallpaperFocus,
  type WallpaperFocus,
} from "./wallpaperFocus";
import {
  normalizeWallpaperClip,
  parseWallpaperClip,
  type WallpaperClip,
} from "./wallpaperClip";

export type { WallpaperFocus } from "./wallpaperFocus";
export type { WallpaperClip } from "./wallpaperClip";
export {
  DEFAULT_WALLPAPER_FOCUS,
  WALLPAPER_FOCUS_MAX_ZOOM,
  isDefaultWallpaperFocus,
  normalizeWallpaperFocus,
  parseWallpaperFocus,
  wallpaperMediaLayout,
} from "./wallpaperFocus";
export {
  WALLPAPER_CLIP_MIN_DURATION,
  clipsEqual,
  enforceVideoClip,
  formatClipTime,
  normalizeWallpaperClip,
  parseWallpaperClip,
} from "./wallpaperClip";

export type ThemeSkinId =
  | "default"
  | "rose"
  | "gothic"
  | "mist"
  | "ocean"
  | "ember";

export type ThemeSkinAppearance = "auto" | Theme;

export interface ThemeSkinMeta {
  id: ThemeSkinId;
  /** Swatch shown in Settings (accent sample). */
  swatch: string;
  /** Secondary swatch for dual-tone preview. */
  swatchAlt: string;
  /**
   * When not `auto`, selecting the skin also switches dark/light
   * (Dream Skin pins appearance for art that only works in one shell).
   */
  appearance: ThemeSkinAppearance;
}

export const SKIN_STORAGE_KEY = "keencode.skin";
/** Scrim strength over wallpaper only (0 = clear wallpaper, 100 = full dim). */
export const WALLPAPER_SCRIM_STORAGE_KEY = "keencode.wallpaper-scrim";
/** Default matches the built-in gradient at full opacity. */
export const DEFAULT_WALLPAPER_SCRIM = 100;
export const DEFAULT_SKIN: ThemeSkinId = "default";

/** Accept common image types + short-loop video for wallpaper upload. */
export const WALLPAPER_ACCEPT =
  "image/jpeg,image/png,image/webp,image/gif,image/jpg,video/mp4,video/webm";

/** Longest edge after compress for still images (keeps IDB payload modest). */
export const WALLPAPER_MAX_EDGE = 1920;

/** Max still-image blob bytes after JPEG compress (~1.6 MiB payload). */
export const WALLPAPER_MAX_IMAGE_BYTES = 1_600_000;

/** Reject image / gif source files larger than this before decode. */
export const WALLPAPER_MAX_SOURCE_BYTES = 12 * 1024 * 1024;

/** Reject video source files larger than this (videos are stored as-is). */
export const WALLPAPER_MAX_VIDEO_BYTES = 200 * 1024 * 1024;

/** Video mimetypes we accept for wallpaper (browser can autoplay when muted). */
export const WALLPAPER_ALLOWED_VIDEO_MIMES: ReadonlySet<string> = new Set([
  "video/mp4",
  "video/webm",
]);

/** 当前持久化层允许的图片 MIME；静态图会在写入前统一压缩为 JPEG。 */
const WALLPAPER_ALLOWED_IMAGE_MIMES: ReadonlySet<string> = new Set([
  "image/jpeg",
  "image/gif",
]);

/**
 * Built-in packs — ids stable for persistence.
 * All skins use appearance: "auto" so light/dark is user-controlled only
 * (selecting a skin never flips shell mode).
 */
export const THEME_SKINS: readonly ThemeSkinMeta[] = [
  {
    id: "default",
    swatch: "#8aa4ff",
    swatchAlt: "#3d5fd9",
    appearance: "auto",
  },
  {
    id: "rose",
    /* Salon blush — carmine on dusty mauve */
    swatch: "#d4536a",
    swatchAlt: "#9b6b7c",
    appearance: "auto",
  },
  {
    id: "gothic",
    /* Cathedral brass on oxblood — works in light parchment too */
    swatch: "#c4a35a",
    swatchAlt: "#6b2e2a",
    appearance: "auto",
  },
  {
    id: "mist",
    /* Nordic fog — cool sage + slate */
    swatch: "#6f8f8a",
    swatchAlt: "#5a6b78",
    appearance: "auto",
  },
  {
    id: "ocean",
    /* Deep harbor — teal-cyan, distinct from default periwinkle */
    swatch: "#2eb8c7",
    swatchAlt: "#1a5f8a",
    appearance: "auto",
  },
  {
    id: "ember",
    /* Forge coal — copper flame */
    swatch: "#e8893a",
    swatchAlt: "#5c2a18",
    appearance: "auto",
  },
] as const;

const SKIN_IDS = new Set<string>(THEME_SKINS.map((s) => s.id));

export function isThemeSkinId(value: unknown): value is ThemeSkinId {
  return typeof value === "string" && SKIN_IDS.has(value);
}

/** 解析当前主题外观；仅缺失值使用首次启动默认值。 */
export function parseThemeSkin(raw: unknown): ThemeSkinId {
  if (raw === null) return DEFAULT_SKIN;
  if (isThemeSkinId(raw)) return raw;
  throw new Error("主题外观格式无效");
}

/** 返回当前主题外观的元数据。 */
export function getThemeSkinMeta(id: ThemeSkinId): ThemeSkinMeta {
  const meta = THEME_SKINS.find((skin) => skin.id === id);
  if (!meta) throw new Error("主题外观格式无效");
  return meta;
}

export interface SkinStorage {
  /** 读取持久化值。 */
  getItem(key: string): string | null;
  /** 写入持久化值。 */
  setItem(key: string, value: string): void;
}

/** 读取已持久化的主题外观。 */
export function loadSkin(storage: SkinStorage): ThemeSkinId {
  return parseThemeSkin(storage.getItem(SKIN_STORAGE_KEY));
}

/** 校验并持久化当前主题外观。 */
export function saveSkin(storage: SkinStorage, skin: ThemeSkinId): void {
  if (!isThemeSkinId(skin)) throw new Error("主题外观格式无效");
  storage.setItem(SKIN_STORAGE_KEY, skin);
}

/** Minimal DOM surface so unit tests need no jsdom. */
export interface SkinRoot {
  setAttribute(name: string, value: string): void;
  removeAttribute(name: string): void;
  style?: {
    setProperty(name: string, value: string): void;
    removeProperty(name: string): void;
  };
}

/** Apply skin id to documentElement (`data-skin`). */
export function applySkinToDocument(
  skin: ThemeSkinId,
  root: SkinRoot = document.documentElement,
): void {
  if (skin === DEFAULT_SKIN) {
    root.removeAttribute("data-skin");
  } else {
    root.setAttribute("data-skin", skin);
  }
}

/** 解析持久化的壁纸遮罩强度；仅缺失值使用默认值。 */
export function parseWallpaperScrim(raw: unknown): number {
  if (raw === null) return DEFAULT_WALLPAPER_SCRIM;
  if (typeof raw !== "string" || !/^(?:0|[1-9]\d?|100)$/.test(raw)) {
    throw new Error("壁纸遮罩强度格式无效");
  }
  return Number(raw);
}

/** 将界面运行时的遮罩输入收敛到可用范围。 */
function normalizeWallpaperScrim(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_WALLPAPER_SCRIM;
  return Math.max(0, Math.min(100, Math.round(value)));
}

/** 读取持久化的壁纸遮罩强度。 */
export function loadWallpaperScrim(
  storage: SkinStorage = localStorage,
): number {
  return parseWallpaperScrim(storage.getItem(WALLPAPER_SCRIM_STORAGE_KEY));
}

/** 收敛并持久化界面运行时的壁纸遮罩强度。 */
export function saveWallpaperScrim(
  storage: SkinStorage,
  value: number,
): void {
  storage.setItem(
    WALLPAPER_SCRIM_STORAGE_KEY,
    String(normalizeWallpaperScrim(value)),
  );
}

/**
 * Apply scrim strength as CSS vars on the root.
 * Scales the full-window veil, pane/settings fills, and sidebar blur.
 * Text and control fills stay solid (they do not read these variables).
 *
 * Derived mix/%/px vars avoid flaky `calc(% * var)` inside `color-mix`
 * in some WebViews. At 0, also sets `data-wallpaper-clear` so CSS can force
 * fully transparent pane fills (some engines leave a residual from 0% mix).
 */
export function applyWallpaperScrimToDocument(
  value: number,
  root: SkinRoot = document.documentElement,
): void {
  const next = normalizeWallpaperScrim(value);
  const t = next / 100;
  if (next <= 0) {
    root.setAttribute("data-wallpaper-clear", "1");
  } else {
    root.removeAttribute("data-wallpaper-clear");
  }
  if (!root.style?.setProperty) return;
  root.style.setProperty("--wallpaper-scrim-opacity", t.toFixed(3));
  root.style.setProperty(
    "--wallpaper-mix-sidebar",
    `${Math.round(58 * t)}%`,
  );
  root.style.setProperty("--wallpaper-mix-main", `${Math.round(70 * t)}%`);
  root.style.setProperty("--wallpaper-mix-aside", `${Math.round(70 * t)}%`);
  root.style.setProperty(
    "--wallpaper-mix-settings",
    `${Math.round(78 * t)}%`,
  );
  root.style.setProperty(
    "--wallpaper-sidebar-blur",
    `${(22 * t).toFixed(1)}px`,
  );
}

/**
 * Resolve whether picking this skin should also flip dark/light.
 * `auto` → null (keep current theme).
 */
export function skinPreferredTheme(skin: ThemeSkinId): Theme | null {
  const appearance = getThemeSkinMeta(skin).appearance;
  if (appearance === "auto") return null;
  return appearance;
}

/* ═══════════════════════════════════════════════════════════════════════════
 * Wallpaper — Blob 与元数据作为一个 IndexedDB 记录持久化。
 * ═══════════════════════════════════════════════════════════════════════════ */

export type WallpaperKind = "image" | "video";

export interface WallpaperMeta {
  kind: WallpaperKind;
  /** 原始 MIME，例如 image/jpeg、image/gif 或 video/mp4。 */
  mime: string;
  /** 设置页展示的原始文件名。 */
  name: string;
  /** 写入时的毫秒时间戳。 */
  createdAt: number;
  /**
   * 上传或首次解码时探测到的媒体固有像素。
   * 渲染层可在视频元数据到达前计算焦点布局，避免冷启动闪动。
   */
  width?: number;
  height?: number;
  /**
   * 可选的平移与缩放焦点；缺失时使用居中 cover。
   * 焦点与 Blob 一同保存在同一个 IndexedDB 完整记录中。
   */
  focus?: WallpaperFocus;
  /**
   * 可选的视频入点和出点，单位为秒；缺失时播放完整视频。
   * 播放仅在区间内跳转，不重新编码源文件。
   */
  clip?: WallpaperClip;
}

export interface WallpaperRecord extends WallpaperMeta {
  blob: Blob;
}

/**
 * 壁纸唯一持久化接口；读取、替换、更新和删除都针对完整记录。
 */
export interface WallpaperStorage {
  /** 读取当前完整壁纸记录。 */
  get(): Promise<WallpaperRecord | null>;
  /** 原子替换当前完整壁纸记录。 */
  set(record: WallpaperRecord): Promise<void>;
  /** 在同一个存储事务中读取并更新当前完整记录。 */
  update(
    updater: (current: WallpaperRecord | null) => WallpaperRecord | null,
  ): Promise<WallpaperRecord | null>;
  /** 删除当前完整壁纸记录。 */
  clear(): Promise<void>;
}

const IDB_NAME = "keencode";
const IDB_VERSION = 1;
const IDB_STORE = "wallpaper";
const IDB_KEY = "current";

/** 为 IndexedDB 错误补充稳定的操作说明。 */
function wallpaperStorageError(
  operation: string,
  cause: unknown,
): Error {
  const detail =
    cause instanceof Error && cause.message ? `：${cause.message}` : "";
  return new Error(`壁纸存储${operation}失败${detail}`, { cause });
}

/** 严格打开当前壁纸数据库；不支持、阻塞或升级失败都直接拒绝。 */
function openWallpaperIdb(factory: IDBFactory | null): Promise<IDBDatabase> {
  if (!factory) {
    return Promise.reject(wallpaperStorageError("打开", new Error("当前环境不支持 IndexedDB")));
  }
  return new Promise((resolve, reject) => {
    let settled = false;
    const rejectOnce = (cause: unknown) => {
      if (settled) return;
      settled = true;
      reject(wallpaperStorageError("打开", cause));
    };
    let request: IDBOpenDBRequest;
    try {
      request = factory.open(IDB_NAME, IDB_VERSION);
    } catch (error) {
      rejectOnce(error);
      return;
    }
    request.onupgradeneeded = () => {
      try {
        const database = request.result;
        if (!database.objectStoreNames.contains(IDB_STORE)) {
          database.createObjectStore(IDB_STORE);
        }
      } catch (error) {
        request.transaction?.abort();
        rejectOnce(error);
      }
    };
    request.onerror = () => rejectOnce(request.error ?? new Error("打开请求失败"));
    request.onblocked = () => rejectOnce(new Error("数据库升级被其他窗口阻塞"));
    request.onsuccess = () => {
      if (settled) {
        request.result.close();
        return;
      }
      if (!request.result.objectStoreNames.contains(IDB_STORE)) {
        request.result.close();
        rejectOnce(new Error("缺少当前壁纸对象仓库"));
        return;
      }
      settled = true;
      resolve(request.result);
    };
  });
}

/** 在一个 IndexedDB 事务中执行单个请求，并严格传播全部失败。 */
function runWallpaperRequest<T>(
  db: IDBDatabase,
  mode: IDBTransactionMode,
  run: (store: IDBObjectStore) => IDBRequest<T>,
  operation: string,
): Promise<T> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const rejectOnce = (cause: unknown) => {
      if (settled) return;
      settled = true;
      reject(wallpaperStorageError(operation, cause));
    };
    let transaction: IDBTransaction;
    let request: IDBRequest<T>;
    try {
      transaction = db.transaction(IDB_STORE, mode);
      request = run(transaction.objectStore(IDB_STORE));
    } catch (error) {
      rejectOnce(error);
      return;
    }
    request.onerror = () => rejectOnce(request.error ?? new Error("存储请求失败"));
    transaction.onerror = () =>
      rejectOnce(transaction.error ?? request.error ?? new Error("事务失败"));
    transaction.onabort = () =>
      rejectOnce(transaction.error ?? request.error ?? new Error("事务已中止"));
    transaction.oncomplete = () => {
      if (settled) return;
      settled = true;
      resolve(request.result);
    };
  });
}

/** 在一个读写事务中严格执行壁纸记录更新。 */
function runWallpaperUpdate(
  db: IDBDatabase,
  updater: (current: WallpaperRecord | null) => WallpaperRecord | null,
): Promise<WallpaperRecord | null> {
  return new Promise((resolve, reject) => {
    let settled = false;
    let next: WallpaperRecord | null = null;
    const rejectOnce = (cause: unknown) => {
      if (settled) return;
      settled = true;
      reject(wallpaperStorageError("更新", cause));
    };
    let transaction: IDBTransaction;
    let request: IDBRequest<unknown>;
    try {
      transaction = db.transaction(IDB_STORE, "readwrite");
      const store = transaction.objectStore(IDB_STORE);
      request = store.get(IDB_KEY);
      request.onsuccess = () => {
        try {
          const current =
            request.result === undefined
              ? null
              : parseWallpaperRecord(request.result);
          const updated = updater(current);
          next = updated ? parseWallpaperRecord(updated) : null;
          const mutation = next
            ? store.put(next, IDB_KEY)
            : store.delete(IDB_KEY);
          mutation.onerror = () =>
            rejectOnce(mutation.error ?? new Error("更新请求失败"));
        } catch (error) {
          transaction.abort();
          rejectOnce(error);
        }
      };
    } catch (error) {
      rejectOnce(error);
      return;
    }
    request.onerror = () => rejectOnce(request.error ?? new Error("读取请求失败"));
    transaction.onerror = () =>
      rejectOnce(transaction.error ?? request.error ?? new Error("事务失败"));
    transaction.onabort = () =>
      rejectOnce(transaction.error ?? request.error ?? new Error("事务已中止"));
    transaction.oncomplete = () => {
      if (settled) return;
      settled = true;
      resolve(next);
    };
  });
}

/** 使用指定 IndexedDB 工厂创建壁纸唯一事实源。 */
export function createIndexedDbWallpaperStorage(
  factory: IDBFactory | null = globalThis.indexedDB ?? null,
): WallpaperStorage {
  return {
    async get() {
      const database = await openWallpaperIdb(factory);
      try {
        const raw = await runWallpaperRequest<unknown>(
          database,
          "readonly",
          (store) => store.get(IDB_KEY),
          "读取",
        );
        return raw === undefined ? null : parseWallpaperRecord(raw);
      } finally {
        database.close();
      }
    },
    async set(record) {
      const validated = parseWallpaperRecord(record);
      const database = await openWallpaperIdb(factory);
      try {
        await runWallpaperRequest(
          database,
          "readwrite",
          (store) => store.put(validated, IDB_KEY),
          "写入",
        );
      } finally {
        database.close();
      }
    },
    async update(updater) {
      const database = await openWallpaperIdb(factory);
      try {
        return await runWallpaperUpdate(database, updater);
      } finally {
        database.close();
      }
    },
    async clear() {
      const database = await openWallpaperIdb(factory);
      try {
        await runWallpaperRequest(
          database,
          "readwrite",
          (store) => store.delete(IDB_KEY),
          "删除",
        );
      } finally {
        database.close();
      }
    },
  };
}

/** 浏览器运行时使用的 IndexedDB 壁纸唯一事实源。 */
export const idbWallpaperStorage = createIndexedDbWallpaperStorage();

/** 创建测试使用的内存壁纸事实源。 */
export function memoryWallpaperStorage(): WallpaperStorage & {
  /** 当前内存记录。 */
  _record: WallpaperRecord | null;
} {
  let current: WallpaperRecord | null = null;
  const storage: WallpaperStorage & { _record: WallpaperRecord | null } = {
    _record: null,
    async get() {
      return current;
    },
    async set(record) {
      current = parseWallpaperRecord(record);
      storage._record = current;
    },
    async update(updater) {
      const updated = updater(current);
      current = updated ? parseWallpaperRecord(updated) : null;
      storage._record = current;
      return current;
    },
    async clear() {
      current = null;
      storage._record = null;
    },
  };
  return storage;
}

interface WallpaperOptions {
  /** 覆盖默认壁纸事实源，供测试注入。 */
  storage?: WallpaperStorage;
}

/** 返回调用方指定或浏览器默认的壁纸事实源。 */
function optsStorage(opts?: WallpaperOptions): WallpaperStorage {
  return opts?.storage ?? idbWallpaperStorage;
}

/** 断言对象只包含当前壁纸元数据字段。 */
function assertWallpaperKeys(
  value: Record<string, unknown>,
  allowed: readonly string[],
  label: string,
): void {
  const allowedKeys = new Set(allowed);
  const unknown = Object.keys(value).find((key) => !allowedKeys.has(key));
  if (unknown) throw new Error(`${label} 包含未知字段：${unknown}`);
}

/** 严格解析壁纸焦点，不对持久化数据做夹取或补默认值。 */
function parseStoredWallpaperFocus(value: unknown): WallpaperFocus {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("壁纸焦点必须是对象");
  }
  const candidate = value as Record<string, unknown>;
  assertWallpaperKeys(candidate, ["cx", "cy", "zoom"], "壁纸焦点");
  if (
    typeof candidate.cx !== "number" ||
    typeof candidate.cy !== "number" ||
    typeof candidate.zoom !== "number" ||
    !Number.isFinite(candidate.cx) ||
    !Number.isFinite(candidate.cy) ||
    !Number.isFinite(candidate.zoom)
  ) {
    throw new Error("壁纸焦点数值无效");
  }
  const parsed = parseWallpaperFocus(candidate);
  if (
    parsed.cx !== candidate.cx ||
    parsed.cy !== candidate.cy ||
    parsed.zoom !== candidate.zoom
  ) {
    throw new Error("壁纸焦点超出范围");
  }
  return parsed;
}

/** 严格解析壁纸视频片段，不修正已存储的边界。 */
function parseStoredWallpaperClip(value: unknown): WallpaperClip {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("壁纸视频片段必须是对象");
  }
  const candidate = value as Record<string, unknown>;
  assertWallpaperKeys(candidate, ["start", "end"], "壁纸视频片段");
  const parsed = parseWallpaperClip(candidate);
  if (
    !parsed ||
    parsed.start !== candidate.start ||
    parsed.end !== candidate.end
  ) {
    throw new Error("壁纸视频片段数值无效");
  }
  return parsed;
}

/** 严格解析当前唯一的壁纸元数据结构。 */
function parseWallpaperMeta(value: unknown): WallpaperMeta {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("壁纸元数据必须是对象");
  }
  const v = value as Record<string, unknown>;
  assertWallpaperKeys(
    v,
    ["kind", "mime", "name", "createdAt", "width", "height", "focus", "clip"],
    "壁纸元数据",
  );
  const kind = v.kind;
  const mime = v.mime;
  const name = v.name;
  const createdAt = v.createdAt;
  if (kind !== "image" && kind !== "video") {
    throw new Error("壁纸类型无效");
  }
  if (
    typeof mime !== "string" ||
    !mime ||
    mime !== mime.trim() ||
    typeof name !== "string" ||
    !name ||
    name !== name.trim()
  ) {
    throw new Error("壁纸 MIME 或文件名无效");
  }
  if (
    (kind === "image" && !WALLPAPER_ALLOWED_IMAGE_MIMES.has(mime)) ||
    (kind === "video" && !WALLPAPER_ALLOWED_VIDEO_MIMES.has(mime))
  ) {
    throw new Error("壁纸类型与 MIME 不匹配");
  }
  if (
    typeof createdAt !== "number" ||
    !Number.isSafeInteger(createdAt) ||
    createdAt <= 0
  ) {
    throw new Error("壁纸创建时间无效");
  }
  const meta: WallpaperMeta = { kind, mime, name, createdAt };
  const width = v.width;
  const height = v.height;
  const hasWidth = width !== undefined;
  const hasHeight = height !== undefined;
  if (hasWidth !== hasHeight) {
    throw new Error("壁纸宽高必须同时存在");
  }
  if (hasWidth) {
    if (
      typeof width !== "number" ||
      typeof height !== "number" ||
      !Number.isSafeInteger(width) ||
      !Number.isSafeInteger(height) ||
      width <= 0 ||
      height <= 0
    ) {
      throw new Error("壁纸宽高无效");
    }
    meta.width = width;
    meta.height = height;
  }
  if (v.focus !== undefined) {
    meta.focus = parseStoredWallpaperFocus(v.focus);
  }
  if (v.clip !== undefined) {
    if (kind !== "video") throw new Error("图片壁纸不能包含视频片段");
    meta.clip = parseStoredWallpaperClip(v.clip);
  }
  return meta;
}

/** 严格解析包含 Blob 的当前完整壁纸记录。 */
function parseWallpaperRecord(value: unknown): WallpaperRecord {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("壁纸记录必须是对象");
  }
  const candidate = value as Record<string, unknown>;
  assertWallpaperKeys(
    candidate,
    [
      "kind",
      "mime",
      "name",
      "createdAt",
      "width",
      "height",
      "focus",
      "clip",
      "blob",
    ],
    "壁纸记录",
  );
  if (!(candidate.blob instanceof Blob)) {
    throw new Error("壁纸记录缺少有效 Blob");
  }
  const { blob, ...rawMeta } = candidate;
  const meta = parseWallpaperMeta(rawMeta);
  if (blob.type !== meta.mime) {
    throw new Error("壁纸 Blob MIME 与元数据不一致");
  }
  if (blob.size <= 0) {
    throw new Error("壁纸 Blob 不能为空");
  }
  return { ...meta, blob };
}

/** 从唯一事实源读取完整壁纸记录。 */
export async function loadWallpaperRecord(
  opts?: WallpaperOptions,
): Promise<WallpaperRecord | null> {
  return optsStorage(opts).get();
}

/** 原子保存包含 Blob 与元数据的完整壁纸记录。 */
export async function saveWallpaper(
  record: WallpaperRecord,
  opts?: WallpaperOptions,
): Promise<void> {
  await optsStorage(opts).set(record);
}

function cloneWallpaperMetaBase(meta: WallpaperMeta): WallpaperMeta {
  const next: WallpaperMeta = {
    kind: meta.kind,
    mime: meta.mime,
    name: meta.name,
    createdAt: meta.createdAt,
  };
  if (meta.width && meta.height) {
    next.width = meta.width;
    next.height = meta.height;
  }
  if (meta.focus) next.focus = meta.focus;
  if (meta.clip) next.clip = meta.clip;
  return next;
}

export type WallpaperAdjustPatch = {
  focus?: WallpaperFocus | null;
  /**
   * Video clip in seconds. Pass `null` to clear (full video).
   * Omit the field to leave the existing clip unchanged.
   */
  clip?: WallpaperClip | null;
  /** When provided, used to decide if clip is "full" and can be omitted. */
  duration?: number;
};

/**
 * 在事实源事务中更新壁纸焦点与视频片段。
 */
export async function saveWallpaperAdjust(
  patch: WallpaperAdjustPatch,
  opts?: WallpaperOptions,
): Promise<WallpaperMeta | null> {
  const nextRecord = await optsStorage(opts).update((current) => {
    if (!current) return null;
    const next = cloneWallpaperMetaBase(current);

    if (patch.focus !== undefined) {
      delete next.focus;
      const nextFocus = patch.focus
        ? normalizeWallpaperFocus(patch.focus)
        : { ...DEFAULT_WALLPAPER_FOCUS };
      if (
        Math.abs(nextFocus.cx - DEFAULT_WALLPAPER_FOCUS.cx) > 1e-6 ||
        Math.abs(nextFocus.cy - DEFAULT_WALLPAPER_FOCUS.cy) > 1e-6 ||
        Math.abs(nextFocus.zoom - DEFAULT_WALLPAPER_FOCUS.zoom) > 1e-6
      ) {
        next.focus = nextFocus;
      }
    }

    if (patch.clip !== undefined) {
      delete next.clip;
      if (patch.clip) {
        const normalized =
          typeof patch.duration === "number" && patch.duration > 0
            ? normalizeWallpaperClip(patch.clip, patch.duration)
            : parseWallpaperClip(patch.clip);
        if (normalized) next.clip = normalized;
      }
    }

    return { ...next, blob: current.blob };
  });
  return nextRecord ? cloneWallpaperMetaBase(nextRecord) : null;
}

/**
 * 首次成功读取尺寸后更新完整记录，后续挂载无需等待视频元数据即可布局。
 */
export async function saveWallpaperMediaSize(
  width: number,
  height: number,
  opts?: WallpaperOptions,
): Promise<WallpaperMeta | null> {
  const nextRecord = await optsStorage(opts).update((current) => {
    if (!current) return null;
    if (
      !Number.isFinite(width) ||
      !Number.isFinite(height) ||
      width <= 0 ||
      height <= 0
    ) {
      return current;
    }
    const w = Math.round(width);
    const h = Math.round(height);
    if (current.width === w && current.height === h) return current;
    const next = cloneWallpaperMetaBase(current);
    next.width = w;
    next.height = h;
    return { ...next, blob: current.blob };
  });
  return nextRecord ? cloneWallpaperMetaBase(nextRecord) : null;
}

/** 从唯一事实源删除完整壁纸记录。 */
export async function clearWallpaper(opts?: WallpaperOptions): Promise<void> {
  await optsStorage(opts).clear();
}

/**
 * Toggle the `data-wallpaper` flag on `<html>`. Called from `main.tsx` for the
 * synchronous boot flag, and from `App.tsx` when the user changes wallpaper.
 * The actual media layer is rendered by React — this only drives CSS
 * (shell transparency + scrim + pane translucency).
 */
export function applyWallpaperFlag(
  present: boolean,
  root: SkinRoot = document.documentElement,
): void {
  if (present) {
    root.setAttribute("data-wallpaper", "1");
  } else {
    root.removeAttribute("data-wallpaper");
  }
}

export type WallpaperPrepareErrorCode =
  | "not_image"
  | "too_large"
  | "decode_failed"
  | "compress_failed"
  | "still_too_large"
  | "unsupported_video"
  | "video_too_large";

export class WallpaperPrepareError extends Error {
  readonly code: WallpaperPrepareErrorCode;
  constructor(code: WallpaperPrepareErrorCode, message?: string) {
    super(message ?? code);
    this.name = "WallpaperPrepareError";
    this.code = code;
  }
}

function readFileAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new WallpaperPrepareError("decode_failed"));
    reader.onload = () => {
      const r = reader.result;
      if (typeof r !== "string") {
        reject(new WallpaperPrepareError("decode_failed"));
        return;
      }
      resolve(r);
    };
    reader.readAsDataURL(file);
  });
}

function loadHtmlImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new WallpaperPrepareError("decode_failed"));
    img.src = src;
  });
}

/** Probe video / gif intrinsic size without fully decoding frames. */
function probeMediaSize(
  blob: Blob,
  kind: "video" | "image",
): Promise<{ width: number; height: number } | null> {
  if (typeof document === "undefined") return Promise.resolve(null);
  const url = URL.createObjectURL(blob);
  return new Promise((resolve) => {
    let settled = false;
    const cleanup = () => {
      try {
        URL.revokeObjectURL(url);
      } catch {
        /* ignore */
      }
    };
    const finish = (size: { width: number; height: number } | null) => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve(size);
    };
    if (kind === "video") {
      const v = document.createElement("video");
      v.preload = "metadata";
      v.muted = true;
      v.playsInline = true;
      const done = (size: { width: number; height: number } | null) => {
        v.onloadedmetadata = null;
        v.onerror = null;
        v.removeAttribute("src");
        try {
          v.load();
        } catch {
          /* ignore */
        }
        finish(size);
      };
      v.onloadedmetadata = () => {
        const width = v.videoWidth;
        const height = v.videoHeight;
        done(width > 0 && height > 0 ? { width, height } : null);
      };
      v.onerror = () => done(null);
      // Safety timeout so a hung probe never blocks upload forever.
      window.setTimeout(() => done(null), 8000);
      v.src = url;
      return;
    }
    const img = new Image();
    img.onload = () => {
      const width = img.naturalWidth || img.width;
      const height = img.naturalHeight || img.height;
      finish(width > 0 && height > 0 ? { width, height } : null);
    };
    img.onerror = () => finish(null);
    img.src = url;
  });
}

function canvasToBlob(
  canvas: HTMLCanvasElement,
  type: string,
  quality: number,
): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob(
      (b) =>
        b
          ? resolve(b)
          : reject(new WallpaperPrepareError("compress_failed")),
      type,
      quality,
    );
  });
}

/**
 * Read a user-picked image / gif / video and return a durable WallpaperRecord.
 * - video/mp4|webm: stored as-is (browser can't re-encode); size-capped.
 * - image/gif: stored as-is to preserve animation.
 * - other still images: downscaled + JPEG-compressed to cap IDB payload.
 * Throws {@link WallpaperPrepareError} on validation / encode failure.
 */
export async function prepareWallpaperFromFile(file: File): Promise<WallpaperRecord> {
  const type = (file.type || "").toLowerCase();
  const name = file.name || "wallpaper";
  const createdAt = Date.now();

  // Video — store original (no in-browser transcode).
  if (type.startsWith("video/")) {
    if (!WALLPAPER_ALLOWED_VIDEO_MIMES.has(type)) {
      throw new WallpaperPrepareError("unsupported_video");
    }
    if (file.size > WALLPAPER_MAX_VIDEO_BYTES) {
      throw new WallpaperPrepareError("video_too_large");
    }
    const size = await probeMediaSize(file, "video");
    return {
      kind: "video",
      mime: type,
      name,
      createdAt,
      blob: file,
      ...(size
        ? { width: size.width, height: size.height }
        : {}),
    };
  }

  // Animated gif — preserve original blob so frames keep playing.
  if (type === "image/gif") {
    if (file.size > WALLPAPER_MAX_SOURCE_BYTES) {
      throw new WallpaperPrepareError("too_large");
    }
    const size = await probeMediaSize(file, "image");
    return {
      kind: "image",
      mime: type,
      name,
      createdAt,
      blob: file,
      ...(size
        ? { width: size.width, height: size.height }
        : {}),
    };
  }

  // Still image — downscale + JPEG compress.
  const nameOk = /\.(jpe?g|png|webp)$/i.test(name);
  if (type && !type.startsWith("image/")) {
    throw new WallpaperPrepareError("not_image");
  }
  if (!type.startsWith("image/") && !nameOk) {
    throw new WallpaperPrepareError("not_image");
  }
  if (file.size > WALLPAPER_MAX_SOURCE_BYTES) {
    throw new WallpaperPrepareError("too_large");
  }

  const rawUrl = await readFileAsDataUrl(file);
  const img = await loadHtmlImage(rawUrl);
  const w0 = img.naturalWidth || img.width;
  const h0 = img.naturalHeight || img.height;
  if (!w0 || !h0) throw new WallpaperPrepareError("decode_failed");

  const scale = Math.min(1, WALLPAPER_MAX_EDGE / Math.max(w0, h0));
  const w = Math.max(1, Math.round(w0 * scale));
  const h = Math.max(1, Math.round(h0 * scale));

  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new WallpaperPrepareError("compress_failed");
  ctx.drawImage(img, 0, 0, w, h);

  // Prefer JPEG for photos; keep reducing quality until under the byte cap.
  let quality = 0.85;
  let blob = await canvasToBlob(canvas, "image/jpeg", quality);
  while (blob.size > WALLPAPER_MAX_IMAGE_BYTES && quality > 0.45) {
    quality -= 0.1;
    blob = await canvasToBlob(canvas, "image/jpeg", quality);
  }
  if (blob.size > WALLPAPER_MAX_IMAGE_BYTES) {
    throw new WallpaperPrepareError("still_too_large");
  }
  return {
    kind: "image",
    mime: "image/jpeg",
    name,
    createdAt,
    blob,
    width: w,
    height: h,
  };
}
