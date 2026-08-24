/** KeenCode Session 的本地展示偏好。 */
export interface SessionPreference {
  /** 是否在所属分组顶部展示。 */
  pinned: boolean;
  /** 是否从默认会话列表隐藏。 */
  archived: boolean;
  /** 用户自定义或自动生成的任务标题。 */
  title?: string;
  /** 标题来源；manual 绝不允许自动覆盖，message-prefix 允许自动短标题替换。 */
  titleSource?: "automatic" | "manual" | "message-prefix";
}

/** 按 Session 标识保存的本地展示偏好。 */
export type SessionPreferences = Record<string, SessionPreference>;

/** 更新 Session 展示偏好时允许提供的局部字段。 */
export type SessionPreferencePatch = Partial<SessionPreference>;

/** Session 展示偏好的当前本地存储键。 */
export const SESSION_PREFERENCES_KEY = "keencode.session-preferences";

/** 返回浏览器可用的本地存储。 */
function defaultStorage(): Storage | null {
  return typeof localStorage !== "undefined" ? localStorage : null;
}

/** 断言对象只包含当前结构声明的键。 */
function assertExactKeys(
  value: Record<string, unknown>,
  allowed: readonly string[],
  label: string,
): void {
  const allowedKeys = new Set(allowed);
  const unknown = Object.keys(value).find((key) => !allowedKeys.has(key));
  if (unknown) throw new Error(`${label} 包含未知字段：${unknown}`);
}

/** 严格解析单个 Session 展示偏好。 */
function parseSessionPreference(
  sessionId: string,
  value: unknown,
): SessionPreference {
  if (!sessionId || sessionId !== sessionId.trim()) {
    throw new Error("Session 标识不能为空或包含首尾空格");
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`Session 偏好必须是对象：${sessionId}`);
  }
  const candidate = value as Record<string, unknown>;
  assertExactKeys(
    candidate,
    ["pinned", "archived", "title", "titleSource"],
    `Session 偏好 ${sessionId}`,
  );
  if (
    typeof candidate.pinned !== "boolean" ||
    typeof candidate.archived !== "boolean"
  ) {
    throw new Error(`Session 偏好缺少布尔字段：${sessionId}`);
  }
  const hasTitle = candidate.title !== undefined;
  const hasTitleSource = candidate.titleSource !== undefined;
  if (hasTitle !== hasTitleSource) {
    throw new Error(`Session 标题与来源必须同时存在：${sessionId}`);
  }
  if (hasTitle) {
    if (
      typeof candidate.title !== "string" ||
      !candidate.title ||
      candidate.title !== candidate.title.trim() ||
      (candidate.titleSource !== "automatic" &&
        candidate.titleSource !== "manual" &&
        candidate.titleSource !== "message-prefix")
    ) {
      throw new Error(`Session 标题结构无效：${sessionId}`);
    }
    return {
      pinned: candidate.pinned,
      archived: candidate.archived,
      title: candidate.title,
      titleSource: candidate.titleSource,
    };
  }
  return {
    pinned: candidate.pinned,
    archived: candidate.archived,
  };
}

/** 严格解析当前 Session 偏好文档。 */
function parseSessionPreferences(value: unknown): SessionPreferences {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Session 偏好根节点必须是对象");
  }
  const preferences: SessionPreferences = {};
  for (const [sessionId, rawPreference] of Object.entries(value)) {
    Object.defineProperty(preferences, sessionId, {
      configurable: true,
      enumerable: true,
      value: parseSessionPreference(sessionId, rawPreference),
      writable: true,
    });
  }
  return preferences;
}

/** 读取当前 Session 展示偏好；仅无记录时返回首次启动值。 */
export function loadSessionPreferences(
  storage: Storage | null = defaultStorage(),
): SessionPreferences {
  if (!storage) return {};
  const raw = storage.getItem(SESSION_PREFERENCES_KEY);
  if (raw === null) return {};
  if (!raw.trim()) throw new Error("Session 偏好文档不能为空");
  return parseSessionPreferences(JSON.parse(raw));
}

/** 更新一个 Session 的本地展示偏好。 */
export function updateSessionPreference(
  sessionId: string,
  patch: SessionPreferencePatch,
  storage: Storage | null = defaultStorage(),
): SessionPreferences {
  if (!sessionId || sessionId !== sessionId.trim()) {
    throw new Error("Session 标识不能为空或包含首尾空格");
  }
  if (!patch || typeof patch !== "object" || Array.isArray(patch)) {
    throw new Error("Session 偏好更新必须是对象");
  }
  assertExactKeys(
    patch as Record<string, unknown>,
    ["pinned", "archived", "title", "titleSource"],
    `Session 偏好更新 ${sessionId}`,
  );
  const preferences = loadSessionPreferences(storage);
  const current = Object.prototype.hasOwnProperty.call(preferences, sessionId)
    ? preferences[sessionId]!
    : {
        pinned: false,
        archived: false,
      };
  const next: SessionPreference = {
    ...current,
    ...patch,
  };
  if (
    current.titleSource === "manual" &&
    patch.titleSource === "automatic"
  ) {
    next.title = current.title;
    next.titleSource = "manual";
  }
  const validated = parseSessionPreference(sessionId, next);
  Object.defineProperty(preferences, sessionId, {
    configurable: true,
    enumerable: true,
    value: validated,
    writable: true,
  });
  if (storage) {
    storage.setItem(SESSION_PREFERENCES_KEY, JSON.stringify(preferences));
  }
  return preferences;
}

/** 永久删除 Session 后移除对应本地展示偏好。 */
export function removeSessionPreference(
  sessionId: string,
  storage: Storage | null = defaultStorage(),
): SessionPreferences {
  if (!sessionId || sessionId !== sessionId.trim()) {
    throw new Error("Session 标识不能为空或包含首尾空格");
  }
  const preferences = loadSessionPreferences(storage);
  if (!Object.prototype.hasOwnProperty.call(preferences, sessionId)) {
    return preferences;
  }
  delete preferences[sessionId];
  if (storage) {
    storage.setItem(SESSION_PREFERENCES_KEY, JSON.stringify(preferences));
  }
  return preferences;
}

/** 将超过保留期且未置顶的对话批量标记为已归档。 */
export function autoArchiveExpiredSessions(
  sessions: readonly { id: string; updatedAt: string }[],
  retentionDays: number,
  now = Date.now(),
  storage: Storage | null = defaultStorage(),
): SessionPreferences {
  const preferences = loadSessionPreferences(storage);
  const cutoff = now - retentionDays * 86_400_000;
  let changed = false;
  for (const session of sessions) {
    const current = preferences[session.id] ?? { pinned: false, archived: false };
    if (!current.pinned && !current.archived && Date.parse(session.updatedAt) <= cutoff) {
      preferences[session.id] = { ...current, archived: true };
      changed = true;
    }
  }
  if (changed && storage) {
    storage.setItem(SESSION_PREFERENCES_KEY, JSON.stringify(preferences));
  }
  return preferences;
}
