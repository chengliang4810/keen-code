import { OnboardingHeader } from "@/onboarding/OnboardingHeader.js";
import { OccupationOnboardingVisual } from "@/onboarding/OccupationOnboardingVisual.js";
import { useOnboardingTrigger } from "@/onboarding/useOnboardingTrigger.js";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { useSettings } from "@/hooks/useSettingService.js";
import { useOnboardingRecordService } from "@/hooks/useOnboardingRecordService.js";
import { usePlatform } from "@/hooks/usePlatform.js";
import { useEffectiveShortcutBindings } from "@/shortcuts/useShortcutBindings.js";
import { matchesShortcutBinding } from "@/shortcuts/bindings.js";
import { useZCodeIntl } from "@/i18n/IntlProvider.js";
import { Button } from "@/components/ui/button.js";
import { Checkbox } from "@/components/ui/checkbox.js";
import { useZCodeStore } from "@/store/StoreProvider.js";
import { logger } from "@/logger.js";
import { DesktopWindowControls } from "@/DesktopWindowControls.js";

/** 本地记录保存有界等待；完成后及时清理超时回调。 */
async function appendOnboardingRecord(
  service: NonNullable<ReturnType<typeof useOnboardingRecordService>>,
  deviceMid: string,
  entry: Parameters<typeof service.appendRecord>[1],
): Promise<void> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      service.appendRecord(deviceMid, entry),
      new Promise((_, reject) => {
        timeout = setTimeout(() => reject(new Error("appendRecord timeout")), 5000);
      }),
    ]);
  } finally {
    clearTimeout(timeout);
  }
}

export function OccupationOnboarding({
  children,
  showWindowControls = false,
  showChildrenWhileLoading = false,
  isMacDesktop,
  isWindowsDesktop,
}: {
  children: ReactNode;
  /** Windows/Linux 自绘窗控：引导全屏覆盖主界面（含标题栏），需在此补最小化/最大化/关闭。 */
  showWindowControls?: boolean;
  /** 独立设置页不依赖引导设置加载，避免应用级引导外层遮住设置内容。 */
  showChildrenWhileLoading?: boolean;
  isMacDesktop?: boolean;
  isWindowsDesktop?: boolean;
}) {
  const { settings, update } = useSettings();
  const platform = usePlatform();
  const onboardingRecord = useOnboardingRecordService();
  const shortcutBindings = useEffectiveShortcutBindings();
  const requested = useZCodeStore((state) => state.newUserOnboardingOpen);
  const setRequested = useZCodeStore((state) => state.setNewUserOnboardingOpen);
  // 引导记录仍由本地服务补全稳定身份。
  const userId = useZCodeStore((state) => state.user?.id) ?? null;
  const { intl } = useZCodeIntl();
  const t = (key: string) => intl.formatMessage({ id: `occupationOnboarding.${key}` });
  // 只保留可配置的记忆偏好；界面模式由 store 固定为 coding。
  const [memory, setMemory] = useState(false);
  const [saving, setSaving] = useState(false);
  const savingRef = useRef(false);
  const [error, setError] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const loadDeviceMid = useCallback(() => platform.getDeviceId(), [platform]);
  const [needsOnboarding, markOnboarded] = useOnboardingTrigger({
    onboardingRecord,
    userId,
    hasStoredOccupation: Boolean(settings?.onboardingOccupation),
    loadDeviceMid,
    update,
  });
  const onboardingVisible = requested || (needsOnboarding === true && !dismissed);
  const closeOnboarding = useCallback(() => {
    if (savingRef.current) return;
    setDismissed(true);
    setRequested(false);
    if (onboardingRecord) {
      void onboardingRecord.dismissOnboarding(platform.getDeviceId()).catch((cause: unknown) => {
        logger.warn("[occupation-onboarding] 写入关闭决策失败", { error: String(cause) });
      });
    }
  }, [onboardingRecord, platform, setRequested]);
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && onboardingVisible && !saving) {
        // 直接退出不改偏好；首次引导会持久化 dismissed，避免下次启动重复展示。
        event.preventDefault();
        event.stopImmediatePropagation();
        closeOnboarding();
        return;
      }
      if (
        !shortcutBindings.openOnboarding.some((binding) => matchesShortcutBinding(event, binding))
      )
        return;
      event.preventDefault();
      event.stopImmediatePropagation();
      if (saving) return;
      // 手动关闭只记录关闭决策，不修改记忆偏好。
      if (onboardingVisible) {
        closeOnboarding();
      } else {
        setRequested(true);
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [
    closeOnboarding,
    shortcutBindings,
    setRequested,
    onboardingVisible,
    saving,
  ]);
  useEffect(() => {
    if (onboardingVisible) setMemory(settings?.memoryEnabled ?? false);
  }, [onboardingVisible]); // eslint-disable-line react-hooks/exhaustive-deps
  if (!settings) return showChildrenWhileLoading ? <>{children}</> : null;
  // 判定进行中先不渲染，避免引导闪现后立即消失（判定为需引导）或先闪引导再进主界面。
  // 来源设置中的 onboardingOccupation 仅保留完成标记；已有标记时不等待 RPC。
  if (!requested && needsOnboarding === null && !settings.onboardingOccupation) return null;
  if (!onboardingVisible) return <>{children}</>;
  const save = async (skip = false) => {
    if (savingRef.current) return;
    savingRef.current = true;
    setSaving(true);
    setError(false);
    try {
      await update({
        // 来源设置字段继续表示引导已完成；不采集职业，记录中的 occupation 为空。
        onboardingOccupation: "other",
        memoryEnabled: skip ? false : memory,
        proactiveSuggestionsEnabled: false,
      });
      // 保存成功就是本次引导的终点；本地记录失败不应留下可再次上报的引导页面。
      setDismissed(true);
      setRequested(false);
      if (onboardingRecord) {
        try {
          // 本地记录固定为编程模式，不写入未经用户选择的工作方向。
          await appendOnboardingRecord(onboardingRecord, platform.getDeviceId(), {
            occupation: null,
            interfaceMode: "coding",
            memoryEnabled: skip ? null : memory,
            proactiveSuggestionsEnabled: false,
            completedAt: new Date().toISOString(),
          });
          markOnboarded();
        } catch (cause) {
          // 偏好已保存成功，记录写失败只留 warn 日志，不打断用户；下次启动按记录会再次触发引导。
          logger.warn("[occupation-onboarding] 写入引导记录失败", { error: String(cause) });
        }
      }
    } catch (cause) {
      logger.warn("[occupation-onboarding] 保存偏好失败", { error: String(cause) });
      setError(true);
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  };
  return (
    <main
      aria-label={t("preferences")}
      data-testid="onboarding-page"
      className="relative flex h-dvh w-full min-h-0 flex-col overflow-hidden bg-background text-foreground"
    >
      <div className="pointer-events-none absolute inset-x-0 top-0 z-20 h-12 [app-region:drag]" />
      {/* 与 Settings 相同，计入 Workspace 的 4px 外层留白、1px 边框和 8px 内边距。 */}
      {showWindowControls ? (
        <div className="absolute right-1 top-1 z-30 mt-px mr-px flex h-12 items-center px-2">
          <DesktopWindowControls />
        </div>
      ) : null}
      <div className="relative grid min-h-0 flex-1 grid-cols-1 gap-0 lg:grid-cols-2 lg:gap-1 lg:p-1">
        <div className="flex min-h-0 flex-col pt-12 [@media(max-height:740px)]:pt-10">
          <OnboardingHeader saving={saving} t={t} onClose={closeOnboarding} />
          <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-6 py-4 sm:px-10">
            {/* 自动外边距让短内容居中，长内容从顶部正常滚动，不影响固定导航。 */}
            <div className="mx-auto my-auto w-full max-w-lg shrink-0">
              <section className="flex w-full flex-col">
                <div className="w-full">
                  <h1 className="text-ui-xl font-semibold tracking-tight text-center">
                    {t("preferences")}
                  </h1>
                  <p className="mx-auto mt-3 max-w-md text-center text-ui-base leading-relaxed text-foreground-subtle">
                    {t("preferencesDescription")}
                  </p>
                  <div className="mt-8 space-y-3">
                    <label className="grid cursor-pointer grid-cols-[auto_1fr] items-center gap-x-4 gap-y-2 rounded-xl border border-card-border bg-card dark:bg-surface/40 p-5 text-ui-base transition-colors hover:bg-surface-hover">
                      <Checkbox checked={memory} disabled={saving} onCheckedChange={(checked) => setMemory(checked === true)} />
                      <span className="font-medium">{t("memory")}</span>
                      <span className="col-start-2 text-ui-sm font-normal text-foreground-subtle">{t("memoryDescription")}</span>
                    </label>
                  </div>
                  {error ? (
                    <p role="alert" className="mt-4 text-ui-sm text-destructive">
                      {t("error")}
                    </p>
                  ) : null}
                </div>
                <footer className="mt-6 flex flex-col gap-3 [@media(max-height:740px)]:mt-4 [@media(max-height:740px)]:gap-1">
                  <Button
                    variant="link"
                    disabled={saving}
                    className="order-2 h-9 self-center rounded-xl px-3 text-ui-base text-foreground-subtle"
                    onClick={() => void save(true)}
                  >
                    {t("skip")}
                  </Button>
                  <div className="flex w-full gap-3">
                    <Button
                      disabled={saving}
                      className="h-11 flex-1 rounded-xl px-5 text-ui-base"
                      onClick={() => void save()}
                    >
                      {t(saving ? "saving" : "start")}
                    </Button>
                  </div>
                </footer>
              </section>
            </div>
          </div>
        </div>
        <OccupationOnboardingVisual
          isMacDesktop={isMacDesktop}
          isWindowsDesktop={isWindowsDesktop}
        />
      </div>
    </main>
  );
}
