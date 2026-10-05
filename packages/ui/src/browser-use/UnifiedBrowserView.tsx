import type { EmbeddedBrowserViewportPreference } from "@zcode/shared";
import { NativeBrowserView } from "@/browser-use/NativeBrowserView.js";

/** 人类浏览器 viewport 偏好变化的来源，供设置持久化层区分写入原因。 */
export type HumanBrowserViewportPreferenceChangeSource = "mode" | "viewport" | "zoom";

/**
 * 浏览器标签的稳定业务输入。Tauri 下页面像素由 Rust child WebView 绘制，
 * renderer 只保留来源视图所需的工具栏、响应式 viewport 和导航回调。
 */
export type UnifiedBrowserViewProps = {
  /** 受控视图 key（= tab.id / sessionId，agent 定位该 tab 用）。 */
  browserKey: string;
  /** 预算恢复时 guest 创建后立即撤销 bootstrap src；保留字段以兼容恢复状态。 */
  isResidencyRestore?: boolean;
  /** pane 是否可见（激活 + 展开）。 */
  isVisible: boolean;
  /** tab strip 选中态与 panel 展示态正交；折叠时仍保留 selected。 */
  isSelected?: boolean;
  /** 当前 task 的 tab 用于原生 WebView 生命周期归属。 */
  isCurrentTask?: boolean;
  /** 挂载时导航到的初始/恢复 URL。 */
  initialUrl?: string | null;
  /** tab shell 当前 favicon；native host 不改动该业务投影。 */
  faviconUrl?: string | null;
  /** 外部请求把该 tab 导航到某 URL。 */
  navigationRequest?: { id: string; url: string } | null;
  /** 当前 URL 变化回传。 */
  onUrlChange?: (url: string) => void;
  /** child WebView 拦截 target=_blank 后交给主界面打开新的浏览器 tab。 */
  onOpenBrowserUrl?: (url: string) => void;
  /** 页面标题/favicon 变化回传。 */
  onPageMetadataChange?: (metadata: { title?: string; faviconUrl?: string | null }) => void;
  /** navigationRequest 消费完回执。 */
  onNavigationRequestHandled?: (requestId: string) => void;
  /** 元素选择上下文兼容字段；Tauri native host 当前不提供页面脚本注入。 */
  workspacePath?: string;
  workspaceIdentity?: string;
  workspaceKey?: string;
  remoteSessionId?: string;
  sessionId?: string;
  /** browser-use runtime 的业务代次；不等同于 Rust child 的 native generation。 */
  browserGeneration?: number;
  residencyGeneration?: number;
  browserUseOperationUntil?: number;
  browserResizeBaselineVersion?: number;
  /** human 空白 tab 延迟创建 child WebView。 */
  deferEmptyGuest?: boolean;
  /** human Browser surface 的响应式 viewport 偏好。 */
  initialHumanViewportPreference?: EmbeddedBrowserViewportPreference;
  /** 只接收 human UI 主动变更。 */
  onHumanViewportPreferenceChange?: (
    preference: EmbeddedBrowserViewportPreference,
    source: HumanBrowserViewportPreferenceChangeSource,
  ) => void;
};

/**
 * 唯一的浏览器 renderer 实现。页面由 Rust child WebView 绘制，
 * renderer 只负责原有工具栏、布局和导航投影。
 */
export function UnifiedBrowserView(props: UnifiedBrowserViewProps): React.JSX.Element {
  return <NativeBrowserView {...props} />;
}
