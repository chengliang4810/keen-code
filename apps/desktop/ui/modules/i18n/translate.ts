import { interpolate, type Locale, type MessageParams } from "./locale";

export function translate(
  locale: Locale,
  message: string,
  params?: MessageParams,
  catalog: Readonly<Record<string, string>> = {},
): string {
  const localized =
    locale === "zh-CN" && Object.prototype.hasOwnProperty.call(catalog, message)
      ? (catalog[message] ?? message)
      : message;
  return interpolate(localized, params);
}
