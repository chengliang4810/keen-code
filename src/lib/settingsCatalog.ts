/** 设置侧栏和深链路由的唯一目录。 */

import type { MessageKey } from "@/i18n";

/** 首版实际展示并允许深链访问的设置分区标识。 */
export type SettingsSectionId =
  | "general"
  | "account"
  | "appearance"
  | "personalization"
  | "archived"
  | "market"
  | "skills"
  | "agents"
  | "mcp"
  | "requests"
  | "analytics"
  | "about";

/** 首版设置侧栏的固定顺序。 */
export const SETTINGS_SECTION_IDS: readonly SettingsSectionId[] = [
  "general",
  "appearance",
  "account",
  "personalization",
  "skills",
  "agents",
  "market",
  "mcp",
  "requests",
  "analytics",
  "archived",
  "about",
] as const;

/** 判断字符串是否为首版可见设置分区。 */
export function isSettingsSectionId(
  value: string | undefined | null,
): value is SettingsSectionId {
  return (
    !!value &&
    (SETTINGS_SECTION_IDS as readonly string[]).includes(value)
  );
}

/** 设置侧栏分组。 */
export type SettingsNavGroup = "core" | "extensions" | "data";

/** 设置侧栏分组顺序与名称。 */
export const SETTINGS_NAV_GROUPS: readonly {
  id: SettingsNavGroup;
  labelKey: MessageKey;
}[] = [
  { id: "core", labelKey: "settings.group.core" },
  { id: "extensions", labelKey: "settings.group.extensions" },
  { id: "data", labelKey: "settings.group.data" },
] as const;

/** 设置侧栏图标标识。 */
export type SettingsNavIcon =
  | "settings"
  | "appearance"
  | "archive"
  | "user"
  | "extensions"
  | "skills"
  | "agents"
  | "mcp"
  | "requests"
  | "info"
  | "personalization"
  | "analytics";

/** 设置侧栏入口定义。 */
export type SettingsNavDef = {
  /** 稳定的可见分区标识。 */
  id: SettingsSectionId;
  /** 侧栏图标标识。 */
  icon: SettingsNavIcon;
  /** 侧栏名称的国际化键。 */
  labelKey: MessageKey;
  /** 侧栏所属分组；null 表示位于所有分组之后的独立入口。 */
  group: SettingsNavGroup | null;
};

/** 首版设置侧栏入口。 */
export const SETTINGS_NAV: readonly SettingsNavDef[] = [
  {
    id: "general",
    icon: "settings",
    labelKey: "settings.nav.general",
    group: "core",
  },
  {
    id: "appearance",
    icon: "appearance",
    labelKey: "settings.nav.appearance",
    group: "core",
  },
  {
    id: "account",
    icon: "user",
    labelKey: "settings.nav.account",
    group: "core",
  },
  {
    id: "personalization",
    icon: "personalization",
    labelKey: "settings.nav.personalization",
    group: "extensions",
  },
  {
    id: "skills",
    icon: "skills",
    labelKey: "ext.skills.title",
    group: "extensions",
  },
  {
    id: "agents",
    icon: "agents",
    labelKey: "agents.title",
    group: "extensions",
  },
  {
    id: "market",
    icon: "extensions",
    labelKey: "ext.market.title",
    group: "extensions",
  },
  {
    id: "mcp",
    icon: "mcp",
    labelKey: "ext.mcp.title",
    group: "extensions",
  },
  {
    id: "requests",
    icon: "requests",
    labelKey: "settings.nav.requests",
    group: "data",
  },
  {
    id: "analytics",
    icon: "analytics",
    labelKey: "settings.nav.analytics",
    group: "data",
  },
  {
    id: "archived",
    icon: "archive",
    labelKey: "settings.nav.archived",
    group: "data",
  },
  {
    id: "about",
    icon: "info",
    labelKey: "settings.nav.about",
    group: null,
  },
];

/** 设置深链位置。 */
export type SettingsLocation = {
  /** 请求访问的当前分区。 */
  section: SettingsSectionId;
  /** 请求滚动到的页面锚点。 */
  anchorId?: string | null;
};

/** 获取首版可见分区的侧栏定义。 */
export function getNavDef(
  section: SettingsSectionId,
): SettingsNavDef | undefined {
  return SETTINGS_NAV.find((item) => item.id === section);
}

/** 只解析当前唯一的 `#/settings/{section}` 路由。 */
export function parseSettingsHash(raw: string): SettingsLocation | null {
  const path = raw.replace(/^#\/?/, "").replace(/\/+$/, "");
  const parts = path.split("/").filter(Boolean);
  if (
    parts.length !== 2 ||
    parts[0] !== "settings" ||
    !isSettingsSectionId(parts[1])
  ) {
    return null;
  }
  return { section: parts[1] };
}

/** 构建当前唯一的设置 Hash。 */
export function buildSettingsHash(location: SettingsLocation): string {
  return `#/settings/${location.section}`;
}

/** 检查设置目录的结构约束，供测试阻止重复或悬空入口。 */
export function catalogInvariants(): string[] {
  const errors: string[] = [];
  const navGroups = new Set<SettingsNavGroup>();
  for (const group of SETTINGS_NAV_GROUPS) {
    if (navGroups.has(group.id)) {
      errors.push(`duplicate nav group: ${group.id}`);
    }
    navGroups.add(group.id);
  }
  const navIds = new Set<string>();
  for (const nav of SETTINGS_NAV) {
    if (navIds.has(nav.id)) errors.push(`duplicate nav section: ${nav.id}`);
    if (nav.group !== null && !navGroups.has(nav.group)) {
      errors.push(`nav section ${nav.id} has unregistered group ${nav.group}`);
    }
    navIds.add(nav.id);
  }
  for (const section of SETTINGS_SECTION_IDS) {
    if (!navIds.has(section)) {
      errors.push(`section id missing from NAV: ${section}`);
    }
  }
  return errors;
}
