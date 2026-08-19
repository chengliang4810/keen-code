/** 设置侧栏、搜索入口和深链路由的唯一目录。 */

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
  "account",
  "appearance",
  "personalization",
  "archived",
  "market",
  "skills",
  "agents",
  "mcp",
  "requests",
  "analytics",
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
export type SettingsNavGroup = "personal" | "system";

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
  /** 侧栏所属分组。 */
  group: SettingsNavGroup;
};

/** 可搜索并可跳转的设置项定义。 */
export type SettingsEntry = {
  /** 设置项的稳定标识。 */
  id: string;
  /** 设置项所属的可见分区。 */
  section: SettingsSectionId;
  /** 页面滚动定位使用的 DOM 标识。 */
  anchorId: string;
  /** 设置项名称的国际化键。 */
  labelKey: MessageKey;
  /** 参与搜索的说明和选项国际化键。 */
  descKeys?: readonly MessageKey[];
  /** 参与搜索的额外别名。 */
  keywords?: readonly string[];
};

/** 首版设置侧栏入口。 */
export const SETTINGS_NAV: readonly SettingsNavDef[] = [
  {
    id: "general",
    icon: "settings",
    labelKey: "settings.nav.general",
    group: "personal",
  },
  {
    id: "account",
    icon: "user",
    labelKey: "settings.nav.account",
    group: "personal",
  },
  {
    id: "appearance",
    icon: "appearance",
    labelKey: "settings.nav.appearance",
    group: "personal",
  },
  {
    id: "personalization",
    icon: "personalization",
    labelKey: "settings.nav.personalization",
    group: "personal",
  },
  {
    id: "archived",
    icon: "archive",
    labelKey: "settings.nav.archived",
    group: "personal",
  },
  {
    id: "market",
    icon: "extensions",
    labelKey: "ext.market.title",
    group: "system",
  },
  {
    id: "skills",
    icon: "skills",
    labelKey: "ext.skills.title",
    group: "system",
  },
  {
    id: "agents",
    icon: "agents",
    labelKey: "agents.title",
    group: "system",
  },
  {
    id: "mcp",
    icon: "mcp",
    labelKey: "ext.mcp.title",
    group: "system",
  },
  {
    id: "requests",
    icon: "requests",
    labelKey: "settings.nav.requests",
    group: "system",
  },
  {
    id: "analytics",
    icon: "analytics",
    labelKey: "settings.nav.analytics",
    group: "system",
  },
  {
    id: "about",
    icon: "info",
    labelKey: "settings.nav.about",
    group: "system",
  },
];

/** 首版所有可搜索设置项。 */
export const SETTINGS_ENTRIES: readonly SettingsEntry[] = [
  {
    id: "general.interfaceLanguage",
    section: "general",
    anchorId: "settings-anchor-interface-language",
    labelKey: "settings.interfaceLanguage",
    descKeys: ["settings.interfaceLanguageDesc"],
    keywords: ["language", "locale", "语言", "語言"],
  },
  {
    id: "archived.conversations",
    section: "archived",
    anchorId: "settings-anchor-archived-conversations",
    labelKey: "settings.nav.archived",
    descKeys: ["settings.archived.desc"],
    keywords: ["archive", "archived", "restore", "归档", "恢复"],
  },
  {
    id: "general.hardwareAcceleration",
    section: "general",
    anchorId: "settings-anchor-hardware-acceleration",
    labelKey: "settings.chromeHardwareAcceleration",
    descKeys: ["settings.chromeHardwareAccelerationDesc"],
    keywords: ["chrome", "gpu", "hardware acceleration", "硬件加速"],
  },
  {
    id: "general.taskNotifications",
    section: "general",
    anchorId: "settings-anchor-task-notifications",
    labelKey: "settings.taskNotifications",
    descKeys: ["settings.taskNotificationsDesc"],
    keywords: ["notification", "task", "通知", "任务"],
  },
  {
    id: "general.notificationSound",
    section: "general",
    anchorId: "settings-anchor-notification-sound",
    labelKey: "settings.notificationSound",
    descKeys: ["settings.notificationSoundDesc"],
    keywords: ["notification", "sound", "通知", "声音"],
  },
  {
    id: "general.keepComputerAwake",
    section: "general",
    anchorId: "settings-anchor-keep-awake",
    labelKey: "settings.keepComputerAwake",
    descKeys: ["settings.keepComputerAwakeDesc"],
    keywords: ["sleep", "awake", "休眠", "运行"],
  },
  {
    id: "general.showFullThinking",
    section: "general",
    anchorId: "settings-anchor-show-full-thinking",
    labelKey: "settings.showFullThinking",
    descKeys: ["settings.showFullThinkingDesc"],
    keywords: ["thinking", "reasoning", "思考过程"],
  },
  {
    id: "account.providers",
    section: "account",
    anchorId: "settings-anchor-account-providers",
    labelKey: "settings.tabProviders",
    descKeys: ["settings.tabProvidersHint"],
    keywords: [
      "provider",
      "relay",
      "custom api",
      "base url",
      "model",
    ],
  },
  {
    id: "appearance.theme",
    section: "appearance",
    anchorId: "settings-anchor-theme",
    labelKey: "settings.theme",
    descKeys: [
      "settings.themeDesc",
      "settings.themeSystem",
      "settings.themeLight",
      "settings.themeDark",
    ],
    keywords: ["theme", "dark", "light", "system", "auto", "跟随系统"],
  },
  {
    id: "appearance.skin",
    section: "appearance",
    anchorId: "settings-anchor-skin",
    labelKey: "settings.skin",
    descKeys: ["settings.skinDesc"],
    keywords: ["skin", "color pack", "accent", "皮肤包"],
  },
  {
    id: "appearance.wallpaper",
    section: "appearance",
    anchorId: "settings-anchor-wallpaper",
    labelKey: "settings.wallpaper",
    descKeys: [
      "settings.wallpaperDesc",
      "settings.wallpaperScrim",
      "settings.wallpaperScrimDesc",
    ],
    keywords: ["wallpaper", "background", "image", "video", "壁纸"],
  },
  {
    id: "ext.market",
    section: "market",
    anchorId: "settings-anchor-ext-market",
    labelKey: "ext.market.title",
    keywords: ["marketplace", "market", "install plugin", "插件市场"],
  },
  {
    id: "ext.plugins",
    section: "market",
    anchorId: "settings-anchor-ext-plugins",
    labelKey: "ext.plugins.title",
    descKeys: ["ext.lead", "ext.plugins.installLabel"],
    keywords: ["plugin", "plugins", "extensions", "插件"],
  },
  {
    id: "ext.skills",
    section: "skills",
    anchorId: "settings-anchor-ext-skills",
    labelKey: "ext.skills.title",
    keywords: ["skill", "skills", "slash", "技能"],
  },
  {
    id: "ext.mcp",
    section: "mcp",
    anchorId: "settings-anchor-ext-mcp",
    labelKey: "ext.mcp.title",
    descKeys: ["ext.mcp.doctor", "ext.mcp.add"],
    keywords: ["mcp", "model context protocol", "server"],
  },
  {
    id: "agents.manage",
    section: "agents",
    anchorId: "settings-anchor-agents",
    labelKey: "agents.title",
    descKeys: ["agents.lead", "agents.add"],
    keywords: ["agent", "subagent", "子智能体", "代理"],
  },
  {
    id: "about.app",
    section: "about",
    anchorId: "settings-anchor-about",
    labelKey: "settings.aboutApp",
    keywords: ["about", "version", "license", "关于", "版本"],
  },
  {
    id: "personalization.customInstructions",
    section: "personalization",
    anchorId: "settings-anchor-personalization",
    labelKey: "settings.personalization.customInstructions",
    descKeys: [
      "settings.personalization.description",
      "settings.personalization.learnMore",
    ],
    keywords: ["custom instructions", "personalization", "自定义指令", "个性化"],
  },
  {
    id: "analytics.usage",
    section: "analytics",
    anchorId: "settings-anchor-analytics",
    labelKey: "settings.nav.analytics",
    descKeys: ["settings.analytics.byModel", "settings.analytics.byDay"],
    keywords: ["usage", "analytics", "tokens", "用量", "统计"],
  },
  {
    id: "requests.history",
    section: "requests",
    anchorId: "settings-anchor-requests",
    labelKey: "settings.nav.requests",
    descKeys: ["settings.requests.description"],
    keywords: ["requests", "history", "provider", "请求", "记录", "日志"],
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
  const navIds = new Set<string>();
  for (const nav of SETTINGS_NAV) {
    if (navIds.has(nav.id)) errors.push(`duplicate nav section: ${nav.id}`);
    navIds.add(nav.id);
  }
  for (const section of SETTINGS_SECTION_IDS) {
    if (!navIds.has(section)) {
      errors.push(`section id missing from NAV: ${section}`);
    }
  }
  const entryIds = new Set<string>();
  const anchors = new Set<string>();
  for (const entry of SETTINGS_ENTRIES) {
    if (entryIds.has(entry.id)) {
      errors.push(`duplicate entry id: ${entry.id}`);
    }
    entryIds.add(entry.id);
    if (anchors.has(entry.anchorId)) {
      errors.push(`duplicate anchor: ${entry.anchorId}`);
    }
    anchors.add(entry.anchorId);
    if (!navIds.has(entry.section)) {
      errors.push(`entry ${entry.id} unknown section ${entry.section}`);
    }
  }
  return errors;
}
