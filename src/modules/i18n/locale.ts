export type Locale = "en-US" | "zh-CN";
export type LanguagePreference = "system" | Locale;
export type MessageParams = Readonly<
  Record<string, string | number | boolean | null | undefined>
>;

/** 旧设置或未知语言回到系统语言，避免持久化无效选项。 */
export function normalizeLanguagePreference(
  value: unknown,
): LanguagePreference {
  return value === "en-US" || value === "zh-CN" ? value : "system";
}

export function resolveLocale(
  preference: LanguagePreference,
  languages: readonly string[],
): Locale {
  if (preference !== "system") return preference;
  for (const language of languages) {
    const primary = language.toLowerCase().split(/[-_]/)[0];
    if (primary === "zh") return "zh-CN";
    if (primary === "en") return "en-US";
  }
  return "en-US";
}

/** 单次替换占位符，参数中的花括号不会被再次解释，缺失参数保留原样。 */
export function interpolate(
  message: string,
  params: MessageParams = {},
): string {
  return message.replace(/\{([a-zA-Z][\w]*)\}/g, (match, key: string) =>
    Object.prototype.hasOwnProperty.call(params, key)
      ? String(params[key])
      : match,
  );
}
