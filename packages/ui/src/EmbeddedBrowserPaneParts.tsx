import type { FormEvent, ReactNode } from "react";
import {
  Bug,
  ChevronLeft,
  ChevronRight,
  Ellipsis,
  ExternalLink,
  Globe,
  LoaderIcon,
  MonitorSmartphone,
  MousePointerClick,
  RefreshCw,
  TriangleAlertIcon,
} from "lucide-react";
import {
  TID_BROWSER_ADDRESS_INPUT,
  TID_BROWSER_BACK_BUTTON,
  TID_BROWSER_DEVTOOLS_BUTTON,
  TID_BROWSER_ELEMENT_PICKER_BUTTON,
  TID_BROWSER_FORWARD_BUTTON,
  TID_BROWSER_LOAD_ERROR,
  TID_BROWSER_MORE_BUTTON,
  TID_BROWSER_OPEN_EXTERNAL_ITEM,
  TID_BROWSER_REFRESH_BUTTON,
  TID_BROWSER_RESPONSIVE_BUTTON,
} from "@zcode/shared";
import { ControlHintTooltip } from "@/ControlHintTooltip.js";
import { Button } from "@/components/ui/button.js";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu.js";
import { Input } from "@/components/ui/input.js";
import {
  isDefaultBrowserOpenableUrl,
  type BrowserState,
} from "@/embeddedBrowserHelpers.js";
import { runUserAction } from "@/lib/userActionTelemetry.js";

export function BrowserToolbar({
  addressValue,
  browserState,
  formatMessage,
  onAddressChange,
  onGoBack,
  onGoForward,
  onOpenExternal,
  onOpenDevTools,
  onPickElement,
  onReload,
  onToggleResponsiveMode,
  onSubmit,
  isElementPickerActive,
  isResponsiveMode,
  showElementPicker = true,
  showDevTools = true,
}: {
  addressValue: string;
  browserState: BrowserState;
  formatMessage: (descriptor: { id: string }) => string;
  onAddressChange: (value: string) => void;
  onGoBack: () => void;
  onGoForward: () => void;
  onOpenExternal: () => void;
  onOpenDevTools?: () => void;
  onPickElement?: () => void;
  onReload: () => void;
  onToggleResponsiveMode: () => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  isElementPickerActive: boolean;
  isResponsiveMode: boolean;
  /** Tauri 原生子 WebView 没有 renderer 注入脚本时隐藏元素拾取入口。 */
  showElementPicker?: boolean;
  /** 宿主没有安全 DevTools 命令时隐藏入口，避免点击无效按钮。 */
  showDevTools?: boolean;
}) {
  return (
    <form
      onSubmit={(event) =>
        runUserAction({
          input: { featureId: "workbench.browser", action: "navigate", trigger: "keyboard" },
          operation: () => onSubmit(event),
          completed: { resultSource: "optimistic_projection" },
          failureStage: "browser_navigate",
        })
      }
      className="flex items-center h-12 px-3 gap-2"
    >
      <BrowserIconButton
        icon={<ChevronLeft className="h-4 w-4" />}
        title={formatMessage({ id: "browser.back" })}
        disabled={!browserState.canGoBack}
        dataTestId={TID_BROWSER_BACK_BUTTON}
        onClick={onGoBack}
      />
      <BrowserIconButton
        icon={<ChevronRight className="h-4 w-4" />}
        title={formatMessage({ id: "browser.forward" })}
        disabled={!browserState.canGoForward}
        dataTestId={TID_BROWSER_FORWARD_BUTTON}
        onClick={onGoForward}
      />
      <BrowserIconButton
        icon={<RefreshCw className={`h-4 w-4 ${browserState.isLoading ? "animate-spin" : ""}`} />}
        title={formatMessage({ id: "browser.reload" })}
        disabled={!browserState.isReady}
        dataTestId={TID_BROWSER_REFRESH_BUTTON}
        onClick={onReload}
      />
      <Input
        type="text"
        value={addressValue}
        data-testid={TID_BROWSER_ADDRESS_INPUT}
        size="sm"
        className="h-7 rounded-lg"
        onChange={(event) => onAddressChange(event.target.value)}
        placeholder={formatMessage({ id: "browser.addressPlaceholder" })}
        spellCheck={false}
      />
      <BrowserIconButton
        icon={<MonitorSmartphone className="h-4 w-4" />}
        title={formatMessage({
          id: isResponsiveMode ? "browser.responsive.exit" : "browser.responsive.enter",
        })}
        disabled={false}
        dataTestId={TID_BROWSER_RESPONSIVE_BUTTON}
        onClick={onToggleResponsiveMode}
        active={isResponsiveMode}
        pressed={isResponsiveMode}
      />
      {showElementPicker ? (
        <BrowserIconButton
          icon={<MousePointerClick className="h-4 w-4" />}
          title={formatMessage({
            id: isElementPickerActive
              ? "browser.elementPicker.cancel"
              : "browser.elementPicker.start",
          })}
          disabled={!browserState.isReady}
          dataTestId={TID_BROWSER_ELEMENT_PICKER_BUTTON}
          onClick={() => onPickElement?.()}
          active={isElementPickerActive}
        />
      ) : null}
      <BrowserToolbarMoreMenu
        canOpenExternal={
          browserState.isReady && isDefaultBrowserOpenableUrl(browserState.currentUrl)
        }
        canOpenDevTools={showDevTools && browserState.isReady}
        formatMessage={formatMessage}
        onOpenExternal={onOpenExternal}
        onOpenDevTools={onOpenDevTools}
      />
    </form>
  );
}

function BrowserToolbarMoreMenu({
  canOpenDevTools,
  canOpenExternal,
  formatMessage,
  onOpenDevTools,
  onOpenExternal,
}: {
  canOpenDevTools: boolean;
  canOpenExternal: boolean;
  formatMessage: (descriptor: { id: string }) => string;
  onOpenDevTools?: () => void;
  onOpenExternal: () => void;
}) {
  const moreLabel = formatMessage({ id: "browser.more" });
  return (
    <DropdownMenu>
      <ControlHintTooltip title={moreLabel}>
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            size="icon-md"
            variant="ghost"
            aria-label={moreLabel}
            data-testid={TID_BROWSER_MORE_BUTTON}
          >
            <Ellipsis className="size-4" />
          </Button>
        </DropdownMenuTrigger>
      </ControlHintTooltip>
      <DropdownMenuContent align="end" className="w-52">
        <DropdownMenuItem
          data-testid={TID_BROWSER_OPEN_EXTERNAL_ITEM}
          disabled={!canOpenExternal}
          onSelect={onOpenExternal}
        >
          <ExternalLink className="size-4" />
          <span>{formatMessage({ id: "browser.openExternal" })}</span>
        </DropdownMenuItem>
        <DropdownMenuItem
          data-testid={TID_BROWSER_DEVTOOLS_BUTTON}
          disabled={!canOpenDevTools}
          onSelect={() => onOpenDevTools?.()}
        >
          <Bug className="size-4" />
          <span>{formatMessage({ id: "browser.devtools" })}</span>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

export function BrowserEmptyState({
  browserState,
  formatMessage,
  isGuestStarting = !browserState.isReady,
}: {
  browserState: BrowserState;
  formatMessage: (descriptor: { id: string }) => string;
  isGuestStarting?: boolean;
}) {
  return (
    <div className="absolute inset-0 z-10 flex items-center justify-center bg-background px-6">
      <div className="flex max-w-sm flex-col pb-10 items-center text-center">
        {isGuestStarting ? (
          <LoaderIcon className="mb-6 size-16 animate-spin text-foreground opacity-30" />
        ) : (
          <Globe className="mb-6 size-16 text-foreground opacity-30" />
        )}
        <h3 className="text-ui-base font-medium text-foreground">
          {formatMessage({ id: "browser.title" })}
        </h3>
        <p className="mt-2 text-ui-base text-foreground-subtle">
          {formatMessage({ id: "browser.empty" })}
        </p>
      </div>
    </div>
  );
}

/**
 * 页面加载失败时的可读错误态。
 *
 * 原生子 WebView 的导航错误需要在主界面画出可读错误态，并提供真实重试入口。
 */
export function BrowserLoadErrorState({
  errorMessage,
  formatMessage,
  onRetry,
}: {
  errorMessage: string;
  formatMessage: (descriptor: { id: string }, values?: Record<string, number | string>) => string;
  onRetry: () => void;
}) {
  return (
    <div
      data-testid={TID_BROWSER_LOAD_ERROR}
      className="absolute inset-0 z-10 flex items-center justify-center bg-background px-6"
    >
      <div className="flex max-w-sm flex-col items-center pb-10 text-center">
        <TriangleAlertIcon className="mb-6 size-16 text-warning opacity-60" />
        <h3 className="text-ui-base font-medium text-foreground">
          {formatMessage({ id: "browser.loadError.title" })}
        </h3>
        <p className="mt-2 font-mono text-ui-sm break-all text-foreground-subtlest">
          {errorMessage}
        </p>
        <Button type="button" variant="outline" className="mt-6" onClick={onRetry}>
          <RefreshCw className="size-4" />
          {formatMessage({ id: "browser.loadError.retry" })}
        </Button>
      </div>
    </div>
  );
}

function BrowserIconButton({
  dataTestId,
  disabled,
  icon,
  onClick,
  title,
  active = false,
  pressed,
}: {
  dataTestId: string;
  disabled: boolean;
  icon: ReactNode;
  onClick: () => void;
  title: string;
  active?: boolean;
  pressed?: boolean;
}) {
  return (
    <ControlHintTooltip title={title}>
      <Button
        type="button"
        size="icon-md"
        variant={active ? "secondary" : "ghost"}
        aria-label={title}
        aria-pressed={pressed}
        data-testid={dataTestId}
        disabled={disabled}
        onClick={onClick}
      >
        {icon}
      </Button>
    </ControlHintTooltip>
  );
}
