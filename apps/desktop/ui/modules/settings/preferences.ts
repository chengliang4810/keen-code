import { uiState } from "@/lib/uiState";
import { create } from "zustand";
import { normalizeLanguagePreference } from "@/modules/i18n/locale";
import { normalizeEffortColor } from "@/modules/settings/effortColor";
import { normalizeUiFontSize } from "@/modules/settings/uiFontSize";
import {
  DEFAULT_PREFERENCES,
  loadPreferences,
  onPreferencesChange,
  type Preferences,
} from "./store";

type State = Preferences & {
  hydrated: boolean;
  /** Subscribe & hydrate. Idempotent — safe to call from multiple windows. */
  init: () => Promise<void>;
};

let initPromise: Promise<void> | null = null;

const FAST_BG_KIND_KEY = "rcode-ui-bg-kind-shadow";
const FAST_BG_IMAGE_ID_KEY = "rcode-ui-bg-image-shadow";

function mirrorBgFastPath(
  kind: Preferences["backgroundKind"],
  imageId: Preferences["backgroundImageId"],
): void {
  if (typeof window === "undefined") return;
  try {
    uiState.setItem(FAST_BG_KIND_KEY, kind);
    if (imageId) uiState.setItem(FAST_BG_IMAGE_ID_KEY, imageId);
    else uiState.removeItem(FAST_BG_IMAGE_ID_KEY);
  } catch {
    /* ignore */
  }
}

export function readBgFastPath(): {
  active: boolean;
  imageId: string | null;
} {
  if (typeof window === "undefined") return { active: false, imageId: null };
  try {
    const kind = uiState.getItem(FAST_BG_KIND_KEY);
    const imageId = uiState.getItem(FAST_BG_IMAGE_ID_KEY);
    return { active: kind === "image" && !!imageId, imageId };
  } catch {
    return { active: false, imageId: null };
  }
}

export const usePreferencesStore = create<State>((set) => ({
  ...DEFAULT_PREFERENCES,
  hydrated: false,
  init: () => {
    if (initPromise) return initPromise;
    initPromise = (async () => {
      try {
        const prefs = await loadPreferences();
        set({ ...prefs, hydrated: true });
        mirrorBgFastPath(prefs.backgroundKind, prefs.backgroundImageId);
        void onPreferencesChange((key, value) => {
          if (key === "uiLanguage") value = normalizeLanguagePreference(value);
          if (key === "uiFontSize") value = normalizeUiFontSize(value);
          if (key === "effortColor") value = normalizeEffortColor(value);
          set({ [key]: value } as Partial<State>);
          if (key === "backgroundKind" || key === "backgroundImageId") {
            const s = usePreferencesStore.getState();
            mirrorBgFastPath(s.backgroundKind, s.backgroundImageId);
          }
        });
      } catch (e) {
        initPromise = null;
        throw e;
      }
    })();
    return initPromise;
  },
}));
