import { useCallback, useSyncExternalStore } from "react";
import { getLocale, subscribeLocale, translateForLocale } from "./state";
import type { MessageParams } from "./locale";

export { t, formatNumber, getLocale } from "./state";
export type { LanguagePreference, Locale } from "./locale";

/** 订阅语言变化而不重建组件，保留终端、编辑器和聊天的运行状态。 */
export function useLocale() {
  return useSyncExternalStore(subscribeLocale, getLocale, getLocale);
}

/** 翻译函数身份随语言变化，让 React Compiler 与 useMemo 正确更新缓存。 */
export function useTranslation() {
  const locale = useLocale();
  return useCallback(
    (message: string, params?: MessageParams) =>
      translateForLocale(locale, message, params),
    [locale],
  );
}
