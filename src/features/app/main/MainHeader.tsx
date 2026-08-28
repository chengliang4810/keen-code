import type {
  Dispatch,
  MouseEvent as ReactMouseEvent,
  RefObject,
  SetStateAction,
} from "react";
import type { MessageKey, Vars } from "@/i18n";
import type { LayoutPrefs } from "@/lib/layout";
import type { Project, SessionRow } from "@/features/app/models";
import type { SessionSnapshot } from "@/lib/session";
import { Button } from "@/components/ui/button";
import { Tip } from "@/components/ui/tooltip";
import {
  IconMore,
  IconNewChat,
  IconPanel,
  IconPanelRight,
  IconSummary,
} from "@/components/icons";
import { saveLayout } from "@/lib/layout";
import { isPlaceholderSessionTitle } from "@/lib/sessionTitle";

type SetState<T> = Dispatch<SetStateAction<T>>;
type Translator = (key: MessageKey, vars?: Vars) => string;
type NewChat = (
  project?: Project | null,
  options?: { seedDraft?: string },
) => void | Promise<void>;

export interface MainHeaderProps {
  layout: LayoutPrefs;
  setLayout: SetState<LayoutPrefs>;
  useCustomWindowChrome: boolean;
  toggleMaximizeFromTitlebar: () => Promise<void>;
  tr: Translator;
  sessions: SessionRow[];
  session: SessionSnapshot;
  summaryOpen: boolean;
  summaryTriggerRef: RefObject<HTMLButtonElement | null>;
  setSummaryOpen: SetState<boolean>;
  openSessionMenu: (event: ReactMouseEvent, session: SessionRow) => void;
  newChat: NewChat;
}

export function MainHeader({
  layout,
  setLayout,
  useCustomWindowChrome,
  toggleMaximizeFromTitlebar,
  tr,
  sessions,
  session,
  summaryOpen,
  summaryTriggerRef,
  setSummaryOpen,
  openSessionMenu,
  newChat,
}: MainHeaderProps) {
  const current = sessions.find((item) => item.id === session.sessionId);
  const title = current?.title || session.title || "";
  const showTitle = !isPlaceholderSessionTitle(title, [
    tr("session.new"),
    tr("session.placeholderTitle"),
  ]);

  return (
    <div
      className="main__top"
      data-tauri-drag-region
      onDoubleClick={() => {
        if (useCustomWindowChrome) void toggleMaximizeFromTitlebar();
      }}
    >
      <div className="main__title-row" data-tauri-drag-region>
        {layout.sidebarCollapsed && (
          <>
            <Tip label={tr("main.leftPaneShow")}>
              <Button
                type="button"
                className="chrome-btn chrome-btn--traffic main__pane-toggle"
                onClick={() =>
                  setLayout((currentLayout) => {
                    const next = { ...currentLayout, sidebarCollapsed: false };
                    saveLayout(localStorage, next);
                    return next;
                  })
                }
              >
                <IconPanel size={16} />
              </Button>
            </Tip>
            <Tip label={tr("sidebar.newSession")}>
              <Button
                type="button"
                className="chrome-btn chrome-btn--traffic"
                onClick={() => void newChat(null)}
              >
                <IconNewChat size={16} />
              </Button>
            </Tip>
          </>
        )}
        {showTitle ? (
          <>
            <Tip label={title}>
              <h1 className="main__title" data-tauri-drag-region>
                {title}
              </h1>
            </Tip>
            {current && (
              <Tip label={tr("session.menu")}>
                <Button
                  type="button"
                  className="chrome-btn main__title-menu"
                  onClick={(event) => openSessionMenu(event, current)}
                >
                  <IconMore size={16} />
                </Button>
              </Tip>
            )}
          </>
        ) : null}
      </div>

      {session.sessionId ? (
        <div className="main__top-actions">
          <Tip
            label={
              summaryOpen
                ? tr("main.summaryHide")
                : tr("main.summaryShow")
            }
          >
            <Button
              ref={summaryTriggerRef}
              type="button"
              className={
                "chrome-btn main__pane-toggle" +
                (summaryOpen ? " is-on" : "")
              }
              aria-pressed={summaryOpen}
              onClick={() => setSummaryOpen((value) => !value)}
            >
              <IconSummary size={16} />
            </Button>
          </Tip>
          <Tip
            label={
              layout.asideCollapsed
                ? tr("main.rightPaneShow")
                : tr("main.rightPaneHide")
            }
          >
            <Button
              type="button"
              className={
                "chrome-btn main__pane-toggle" +
                (!layout.asideCollapsed ? " is-on" : "")
              }
              onClick={() =>
                setLayout((currentLayout) => {
                  const next = {
                    ...currentLayout,
                    asideCollapsed: !currentLayout.asideCollapsed,
                  };
                  saveLayout(localStorage, next);
                  return next;
                })
              }
            >
              <IconPanelRight size={16} />
            </Button>
          </Tip>
        </div>
      ) : null}
    </div>
  );
}
