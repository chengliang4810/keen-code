import { createContext, useContext, useMemo, type ReactNode } from "react";
import { resolvePath } from "@/modules/ai/tools/context";

type FileActions = {
  workspaceRoot: string | null;
  openFile?: (path: string) => void;
};

const Context = createContext<FileActions>({ workspaceRoot: null });

// 路径跟随所属会话工作区，不能使用已切换的终端 cwd。
export function ConversationFiles({
  workspaceRoot,
  onOpenFile,
  children,
}: {
  workspaceRoot: string | null;
  onOpenFile?: (path: string) => void;
  children: ReactNode;
}) {
  const value = useMemo<FileActions>(
    () => ({
      workspaceRoot,
      openFile:
        onOpenFile && workspaceRoot
          ? (path) => onOpenFile(resolvePath(path, workspaceRoot))
          : undefined,
    }),
    [workspaceRoot, onOpenFile],
  );
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useConversationFiles() {
  return useContext(Context);
}
