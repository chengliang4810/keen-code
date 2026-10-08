import { useEffect, useState, useSyncExternalStore } from "react";
import { native } from "@/modules/ai/lib/native";
import { watchGlobalInstructions } from "@/modules/ai/lib/instructionsWatch";
import { createInstructionEditor } from "@/settings/lib/instructionEditor";

export function useInstructionEditor() {
  const [editor] = useState(() =>
    createInstructionEditor({
      read: async () => (await native.readAgentInstructions(null)).global,
      save: native.saveGlobalInstructions,
    }),
  );
  const state = useSyncExternalStore(editor.subscribe, editor.getSnapshot);
  const [watchError, setWatchError] = useState("");

  useEffect(() => {
    let watching: (() => void) | undefined;
    let watchFailed = false;
    const refresh = () => {
      if (document.hidden) return;
      void editor.refresh();
    };
    const watch = () => {
      watching?.();
      watchFailed = false;
      watching = watchGlobalInstructions(
        () => {
          watchFailed = false;
          setWatchError("");
          refresh();
        },
        (error) => {
          watchFailed = true;
          setWatchError(String(error));
          refresh();
        },
      );
    };
    const resume = () => {
      if (document.hidden) return;
      if (watchFailed) watch();
      else refresh();
    };
    watch();
    window.addEventListener("focus", resume);
    document.addEventListener("visibilitychange", resume);
    return () => {
      watching?.();
      window.removeEventListener("focus", resume);
      document.removeEventListener("visibilitychange", resume);
    };
  }, [editor]);

  return {
    editor,
    state: watchError ? { ...state, error: state.error || watchError } : state,
  };
}
