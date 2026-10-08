import { uiState } from "@/lib/uiState";
import {
  normalizeLanguagePreference,
  resolveLocale,
  type LanguagePreference,
  type Locale,
  type MessageParams,
} from "./locale";
import { translate } from "./translate";

const LANGUAGE_SHADOW_KEY = "rcode-ui-language-shadow";
const listeners = new Set<() => void>();

function systemLanguages(): readonly string[] {
  return typeof navigator === "undefined" ? [] : navigator.languages;
}

function initialPreference(): LanguagePreference {
  try {
    return normalizeLanguagePreference(
      uiState.getItem(LANGUAGE_SHADOW_KEY),
    );
  } catch {
    return "system";
  }
}

let preference = initialPreference();
let locale: Locale = "en-US";
let catalog: Readonly<Record<string, string>> = {};
let chineseCatalog: Promise<void> | null = null;

export function getLocale(): Locale {
  return locale;
}

export function subscribeLocale(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** 这里只缓存无敏感信息的语言选项，正式设置仍由 Tauri Store 持久化。 */
export async function applyLanguagePreference(
  value: LanguagePreference,
): Promise<void> {
  preference = normalizeLanguagePreference(value);
  try {
    uiState.setItem(LANGUAGE_SHADOW_KEY, preference);
  } catch {
    // WebView 禁用本地存储时仍可通过正式偏好存储切换语言。
  }
  await refreshSystemLocale();
}

export async function refreshSystemLocale(): Promise<void> {
  const next = resolveLocale(preference, systemLanguages());
  // 英文界面不加载中文语言包；加载完成后再发布，避免半翻译状态。
  if (next === "zh-CN") {
    chineseCatalog ??= import("./zh-CN").then((module) => {
      catalog = module.zhCN;
    });
    try {
      await chineseCatalog;
    } catch (error) {
      chineseCatalog = null;
      throw error;
    }
    // 加载期间用户可能已切回英文，不覆盖更新后的选择。
    if (resolveLocale(preference, systemLanguages()) !== next) return;
  }
  if (next === locale) return;
  locale = next;
  for (const listener of listeners) listener();
}

export function t(message: string, params?: MessageParams): string {
  return translate(locale, message, params, catalog);
}

export function translateForLocale(
  value: Locale,
  message: string,
  params?: MessageParams,
): string {
  return translate(value, message, params, catalog);
}

export function formatNumber(value: number): string {
  return new Intl.NumberFormat(locale).format(value);
}

void refreshSystemLocale().catch((error) =>
  console.error("language loading failed", error),
);
