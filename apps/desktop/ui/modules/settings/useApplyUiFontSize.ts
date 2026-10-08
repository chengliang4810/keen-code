import { usePreferencesStore } from "@/modules/settings/preferences";
import { useEffect } from "react";

export function useApplyUiFontSize(): void {
  const fontSize = usePreferencesStore((s) => s.uiFontSize);

  useEffect(() => {
    // 只改变文字标尺，根字号维持原值，避免 rem 图标、间距和圆角联动。
    document.documentElement.style.setProperty(
      "--ui-font-size",
      `${fontSize}px`,
    );
  }, [fontSize]);
}
