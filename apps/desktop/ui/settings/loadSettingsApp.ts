let settingsAppPromise:
  | Promise<{ default: typeof import("@/settings/SettingsApp").SettingsApp }>
  | undefined;

/** 预加载与首次打开共用请求；预加载失败后允许打开时重试。 */
export function loadSettingsApp() {
  settingsAppPromise ??= import("@/settings/SettingsApp")
    .then((module) => ({ default: module.SettingsApp }))
    .catch((error: unknown) => {
      settingsAppPromise = undefined;
      throw error;
    });
  return settingsAppPromise;
}
