import type {
  CSSProperties,
  Dispatch,
  RefObject,
  SetStateAction,
} from "react";
import type { MessageKey, Vars } from "@/i18n";
import type { DragZone } from "@/lib/dragZone";
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
import { IconAttach } from "@/components/icons";

type SetState<T> = Dispatch<SetStateAction<T>>;
type Translator = (key: MessageKey, vars?: Vars) => string;

export interface MainStageFrameProps {
  layout: LayoutPrefs;
  setLayout: SetState<LayoutPrefs>;
  dragZone: DragZone;
  toast: string | null;
  tr: Translator;
  composerFloatPad: number;
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
  const {
    layout,
    setLayout,
    dragZone,
    toast,
    tr,
    composerFloatPad,
    streamA11yNote,
  } = stage;
  const welcomeSession = composer.context.welcomeSession;
  const summaryOpen = conversation.summaryOpen;
  return (
    <main
      className={
        "main" +
        (layout.sidebarCollapsed ? " main--sidebar-hidden" : "") +
        (layout.asideCollapsed ? " main--aside-hidden" : "") +
        (dragZone === "main" ? " is-drop-target" : "")
      }
    >
      {dragZone === "main" ? (
        <div className="drop-overlay drop-overlay--attach" aria-hidden>
          <div className="drop-overlay__card">
            <span className="drop-overlay__icon"><IconAttach size={22} /></span>
            <strong>{tr("composer.dropAttachTitle")}</strong>
            <span>{tr("composer.dropAttachHint")}</span>
          </div>
        </div>
      ) : null}
      {toast ? <div className="app-toast" role="status">{toast}</div> : null}

      <MainHeader {...header} layout={layout} setLayout={setLayout} />
      <MainNotices {...notices} />

      <div
        className={
          "main__stage" +
          (summaryOpen ? " main__stage--summary-open" : "")
        }
        style={{
          ["--composer-float-pad" as string]: `${composerFloatPad}px`,
        } as CSSProperties}
      >
        <div className="sr-only" aria-live="polite" aria-atomic="true">
          {streamA11yNote}
        </div>
        <ConversationStage
          {...conversation}
          layout={layout}
          setLayout={setLayout}
        />
        <AskUserPanel {...askUser} />

        <div
          ref={composer.wrapRef}
          className={
            "composer-wrap composer-wrap--float" +
            (welcomeSession ? " composer-wrap--welcome" : "")
          }
        >
          <div
            className={
              "composer-stack" +
              (welcomeSession ? " composer-stack--with-context" : "")
            }
          >
            <ComposerContextBar {...composer.context} />
            <div
              ref={composer.shellRef}
              className={
                "composer" +
                (dragZone === "main" ? " composer--drop-ready" : "")
              }
            >
              <ComposerQueue {...composer.queue} />
              <ComposerAttachments {...composer.attachments} />
              <ComposerInputArea {...composer.input} />
              <ComposerToolbar {...composer.toolbar} />
            </div>
          </div>
        </div>
      </div>
    </main>
  );
}
