// 容器宽度由会话窗格决定，打开开发工具不会误触宽屏布局。
export const CONVERSATION_COLUMN =
  "w-full @min-[864px]/conversation:w-[calc(100%_-_6rem)] @min-[864px]/conversation:max-w-4xl @min-[1280px]/conversation:w-[calc(100%_-_24rem)] @min-[1280px]/conversation:max-w-6xl";

export const CONVERSATION_PANEL_OFFSET =
  "@min-[1280px]/conversation:-translate-x-42";

export type ConversationPanelMode = "auto" | "panel" | "mini";
