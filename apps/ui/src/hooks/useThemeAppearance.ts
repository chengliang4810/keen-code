import { useEffect, useState } from "react";
import {
  applyThemeColors,
  BASE_COLORS,
  DEFAULT_BASE_COLOR,
  DEFAULT_PRIMARY_COLOR,
  DEFAULT_SECONDARY_COLOR,
  PRIMARY_COLORS,
  SECONDARY_COLORS,
  loadColor,
  loadBrandColor,
  type BaseColor,
  type PrimaryColor,
  type SecondaryColor,
} from "@/lib/themeColors";
import {
  applyEffortColor,
  loadEffortColor,
  type EffortColor,
} from "@/lib/effortColor";
import { useTheme } from "@appica/ui-react/hooks/use-theme";
import {
  applyNativeWindowTheme,
  applyThemeToDocument,
  DEFAULT_RESOLVED_THEME,
  DEFAULT_THEME_PREFERENCE,
  isTheme,
  isThemePreference,
  type Theme,
  type ThemePreference,
} from "@/lib/theme";
import {
  applyUiFontSizeToDocument,
  loadUiFontSize,
  saveUiFontSize,
} from "@/lib/uiFontSize";

/**
 * 外观域 hook。
 *
 * 主题状态的权威来源是 Appica 官方 `useTheme()`：provider 负责持久化、
 * 系统跟随、`.dark` 类与防闪脚本。本 hook 只做两件 provider 覆盖不到的事：
 * 把解析后的主题镜像到 KeenCode 自有表面（`data-theme` 属性、meta、
 * Tauri 原生窗口外观），以及管理皮肤与界面字号。
 */
export function useThemeAppearance() {
  const { theme: storedTheme, resolvedTheme, setTheme } = useTheme();
  /** provider 值理论上可为任意字符串；无效值回落默认偏好。 */
  const themePreference: ThemePreference = isThemePreference(storedTheme)
    ? storedTheme
    : DEFAULT_THEME_PREFERENCE;
  /** resolvedTheme 已由 provider 解析为 `light`/`dark`；非 light 一律按 dark。 */
  const theme: Theme = isTheme(resolvedTheme)
    ? resolvedTheme
    : DEFAULT_RESOLVED_THEME;
  const [baseColor, setBaseColor] = useState<BaseColor>(() =>
    loadColor("keencode.base-color", BASE_COLORS, DEFAULT_BASE_COLOR),
  );
  const [primaryColor, setPrimaryColor] = useState<PrimaryColor>(() =>
    loadBrandColor("keencode.primary-color", PRIMARY_COLORS, DEFAULT_PRIMARY_COLOR),
  );
  const [secondaryColor, setSecondaryColor] = useState<SecondaryColor>(() =>
    loadBrandColor("keencode.secondary-color", SECONDARY_COLORS, DEFAULT_SECONDARY_COLOR),
  );
  const [effortColor, setEffortColor] = useState<EffortColor>(() =>
    loadEffortColor(),
  );
  const [uiFontSize, setUiFontSize] = useState(() =>
    loadUiFontSize(localStorage),
  );

  // 产品选择器与 Tauri 原生外观跟随解析结果。
  useEffect(() => {
    applyThemeToDocument(theme);
    void applyNativeWindowTheme(themePreference === "system" ? null : theme);
  }, [theme, themePreference]);

  useEffect(() => {
    applyUiFontSizeToDocument(uiFontSize);
  }, [uiFontSize]);

  useEffect(() => {
    applyThemeColors(baseColor, primaryColor, secondaryColor);
  }, [baseColor, primaryColor, secondaryColor]);

  useEffect(() => {
    applyEffortColor(effortColor);
  }, [effortColor]);

  const applyThemeChoice = (next: ThemePreference) => {
    if (next === "system") {
      // 先解锁 WebView 原生外观并等两帧让 prefers-color-scheme 稳定，
      // 再让 provider 解析 system；直接声明会读到锁定期间的冻结值。
      void applyNativeWindowTheme(null).then(() => {
        if (typeof requestAnimationFrame === "function") {
          requestAnimationFrame(() =>
            requestAnimationFrame(() => setTheme(next)),
          );
        } else {
          setTheme(next);
        }
      });
      return;
    }
    setTheme(next);
  };

  const applyUiFontSizeChoice = (next: number) => {
    saveUiFontSize(localStorage, next);
    applyUiFontSizeToDocument(next);
    setUiFontSize(next);
  };

  return {
    themePreference,
    baseColor,
    primaryColor,
    secondaryColor,
    applyBaseColorChoice: (value: BaseColor) => {
      localStorage.setItem("keencode.base-color", value);
      setBaseColor(value);
    },
    applyPrimaryColorChoice: (value: PrimaryColor) => {
      localStorage.setItem("keencode.primary-color", value);
      setPrimaryColor(value);
    },
    applySecondaryColorChoice: (value: SecondaryColor) => {
      localStorage.setItem("keencode.secondary-color", value);
      setSecondaryColor(value);
    },
    effortColor,
    applyEffortColorChoice: (value: EffortColor) => {
      localStorage.setItem("keencode.effort-color", value);
      setEffortColor(value);
    },
    uiFontSize,
    applyThemeChoice,
    applyUiFontSizeChoice,
  };
}
