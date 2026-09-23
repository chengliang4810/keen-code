import { useToastManager } from "@appica/ui-react/toast";
import type {
  CSSProperties,
  Dispatch,
  RefObject,
  SetStateAction,
} from "react";
import { useEffect, useRef, useState } from "react";
import type { MessageKey, Vars } from "@/i18n";
import type { LayoutPrefs } from "@/lib/layout";
import type { AskUserPanelProps } from "./main/AskUserPanel";
import type { ComposerAttachmentsProps } from "./main/ComposerAttachments";
import type { ComposerContextBarProps } from "./main/ComposerContextBar";
import type { ComposerInputAreaProps } from "./main/ComposerInputArea";
import type { ComposerQueueProps } from "./main/ComposerQueue";
import type { ComposerToolbarProps } from "./main/ComposerToolbar";
import type { ConversationStageProps } from "./main/ConversationStage";
import type { MainHeaderProps } from "./main/MainHeader";
import type { MainNoticesProps } from "./main/MainNotices";
import { MainHeader } from "./main/MainHeader";
import { MainNotices } from "./main/MainNotices";
import { ConversationStage } from "./main/ConversationStage";
import { AskUserPanel } from "./main/AskUserPanel";
import { ComposerContextBar } from "./main/ComposerContextBar";
import { ComposerQueue } from "./main/ComposerQueue";
import { ComposerAttachments } from "./main/ComposerAttachments";
import { ComposerInputArea } from "./main/ComposerInputArea";
import { ComposerToolbar } from "./main/ComposerToolbar";
import { useConversationWidth } from "@/hooks/useConversationWidth";

type SetState<T> = Dispatch<SetStateAction<T>>;
type Translator = (key: MessageKey, vars?: Vars) => string;

// 时间段边界与文案目录一一对应，避免欢迎态跨午夜后继续显示旧问候。
const WELCOME_GREETING_BOUNDARIES = [5, 9, 12, 14, 18, 23] as const;
type WelcomeGreetingKey =
  | "main.greeting.morningEarly"
  | "main.greeting.morning"
  | "main.greeting.noon"
  | "main.greeting.afternoon"
  | "main.greeting.evening"
  | "main.greeting.lateNight";

// 问候只在六个时间段切换；计时器到达下一个边界后立即重新计算本地时间。
function getWelcomeGreetingKey(date: Date = new Date()): WelcomeGreetingKey {
  const hour = date.getHours();
  if (hour >= 5 && hour < 9) return "main.greeting.morningEarly";
  if (hour >= 9 && hour < 12) return "main.greeting.morning";
  if (hour >= 12 && hour < 14) return "main.greeting.noon";
  if (hour >= 14 && hour < 18) return "main.greeting.afternoon";
  if (hour >= 18 && hour < 23) return "main.greeting.evening";
  return "main.greeting.lateNight";
}

function getNextWelcomeGreetingDelayMs(date: Date = new Date()): number {
  const nextBoundary = WELCOME_GREETING_BOUNDARIES.map((hour) => {
    const candidate = new Date(date);
    candidate.setHours(hour, 0, 0, 0);
    return candidate;
  }).find((candidate) => candidate.getTime() > date.getTime());

  if (nextBoundary) return Math.max(1, nextBoundary.getTime() - date.getTime());

  const tomorrow = new Date(date);
  tomorrow.setDate(tomorrow.getDate() + 1);
  tomorrow.setHours(WELCOME_GREETING_BOUNDARIES[0], 0, 0, 0);
  return Math.max(1, tomorrow.getTime() - date.getTime());
}

function WelcomeCopy({ tr }: { tr: Translator }) {
  const [greetingDate, setGreetingDate] = useState(() => new Date());

  useEffect(() => {
    const timer = window.setTimeout(() => {
      setGreetingDate(new Date());
    }, getNextWelcomeGreetingDelayMs(greetingDate));
    return () => window.clearTimeout(timer);
  }, [greetingDate]);

  return (
    <div className="composer-welcome" data-testid="composer-welcome">
      <h1>{tr(getWelcomeGreetingKey(greetingDate))}</h1>
    </div>
  );
}

export interface MainStageFrameProps {
  layout: LayoutPrefs;
  setLayout: SetState<LayoutPrefs>;
  toast: string | null;
  tr: Translator;
  /** 输入区自身高度，不含上方问答卡片。 */
  composerHeight: number;
  streamA11yNote: string;
}

export type MainHeaderRegionProps = Omit<MainHeaderProps, "layout" | "setLayout">;
export type ConversationRegionProps = Omit<
  ConversationStageProps,
  "layout" | "setLayout"
>;

export interface MainComposerProps {
  wrapRef: RefObject<HTMLDivElement | null>;
  shellRef: RefObject<HTMLDivElement | null>;
  context: ComposerContextBarProps;
  queue: ComposerQueueProps;
  attachments: ComposerAttachmentsProps;
  input: ComposerInputAreaProps;
  toolbar: ComposerToolbarProps;
}

/**
 * 中央工作区的跨组件数据契约。
 *
 * 这里仅保留舞台装配所需的状态和动作；状态归约、持久化和协议处理均由
 * hooks/lib 负责，具体区域继续由 `main/` 下的业务组件承载。
 */
export interface MainStageProps {
  stage: MainStageFrameProps;
  header: MainHeaderRegionProps;
  notices: MainNoticesProps;
  conversation: ConversationRegionProps;
  askUser: AskUserPanelProps;
  composer: MainComposerProps;
}

export function MainStage({
  stage,
  header,
  notices,
  conversation,
  askUser,
  composer,
}: MainStageProps) {
  const toastManager = useToastManager();
  /** Appica manager 发布后可能改变引用；同一条业务提示只允许入队一次。 */
  const publishedToastRef = useRef<string | null>(null);
  const {
    layout,
    setLayout,
    toast,
    tr,
    composerHeight,
    streamA11yNote,
  } = stage;
  const welcomeSession = composer.context.welcomeSession;
  const conversationWidthRef = useConversationWidth();
  const summaryOpen = conversation.summaryOpen;
  /** 会话态 composer 由 ConversationThread 挂载到同一滚动视口；草稿态仍留在舞台中居中。 */
  const composerDock = (
    <div
      ref={composer.wrapRef}
      className={
        "composer-wrap " +
        (welcomeSession ? "composer-wrap--welcome" : "composer-wrap--sticky")
      }
    >
      {welcomeSession && conversation.showWelcomeCopy ? (
        <WelcomeCopy tr={tr} />
      ) : null}
      <ComposerQueue {...composer.queue} />
      <div
        className={
          "composer-stack" +
          (welcomeSession ? " composer-stack--with-context" : "")
        }
      >
        <ComposerContextBar {...composer.context} />
        <div
          inert={Boolean(askUser.askUser)}
          ref={composer.shellRef}
          className="composer"
        >
          <ComposerAttachments {...composer.attachments} />
          <ComposerInputArea {...composer.input} />
          <ComposerToolbar {...composer.toolbar} />
        </div>
      </div>
    </div>
  );
  useEffect(() => {
    if (!toast) {
      publishedToastRef.current = null;
      return;
    }
    if (publishedToastRef.current === toast) return;
    publishedToastRef.current = toast;
    toastManager.add({ title: toast, timeout: 2000 });
  }, [toast, toastManager]);
  return (
    <main
      ref={conversationWidthRef}
      className={
        // 桌面无论资源栏是否展开都保留 ZCode conversation frame，窄视口由 CSS 去除桌面框体。
        "main main--frame" +
        (layout.sidebarCollapsed ? " main--sidebar-hidden" : "") +
        (layout.asideCollapsed ? " main--aside-hidden" : "")
      }
    >
      <MainHeader {...header} layout={layout} setLayout={setLayout} />
      <MainNotices {...notices} />

      <div
        className={
          "main__stage" +
          (summaryOpen ? " main__stage--summary-open" : "")
        }
        style={{
          ["--composer-height" as string]: `${composerHeight}px`,
        } as CSSProperties}
      >
        <div className="sr-only" aria-live="polite" aria-atomic="true">
          {streamA11yNote}
        </div>
        <ConversationStage
          {...conversation}
          layout={layout}
          setLayout={setLayout}
          bottomDock={welcomeSession ? null : composerDock}
        />
        <AskUserPanel {...askUser} />
        {welcomeSession ? composerDock : null}
      </div>
    </main>
  );
}
