export interface Project {
  id: string;
  name: string;
  path: string;
  pathOk: boolean;
}

export interface SessionRow {
  id: string;
  title: string;
  projectId: string | null;
  updatedAt: string;
  archived: boolean;
  /** Pinned chats float to the top of the sidebar */
  pinned: boolean;
}

export function projectPathPreview(parent: string, name: string): string {
  const separator = parent.includes("\\") ? "\\" : "/";
  return `${parent.replace(/[/\\]+$/, "")}${separator}${name}`;
}

/** 单个 ACP Session 最近一次上报的上下文使用量。 */
export interface SessionContextUsage {
  /** 当前上下文已使用的 token 数。 */
  used: number;
  /** 当前模型的上下文容量。 */
  size?: number;
  /** true 表示使用量来自本地请求体估算。 */
  estimated: boolean;
}

export type ContextMenuState =
  | { kind: "project"; id: string; x: number; y: number }
  | { kind: "session"; id: string; x: number; y: number }
  | null;

/** In-app dialogs — window.prompt/confirm are unreliable in Tauri WebView. */
export type AppDialog =
  | {
      kind: "confirm";
      title: string;
      message: string;
      confirmLabel?: string;
      danger?: boolean;
      onConfirm: () => void | Promise<void>;
    }
  | {
      kind: "prompt";
      title: string;
      initial: string;
      /** 输入框上方的可选补充说明。 */
      message?: string;
      placeholder?: string;
      /** Primary submit button label (default: common.save). */
      submitLabel?: string;
      onSubmit: (value: string) => void | Promise<void>;
    }
  | null;
