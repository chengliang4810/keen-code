import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";

type DisclosureState = {
  values: ReadonlyMap<string, boolean>;
  setOpen: (id: string, open: boolean) => void;
};

const Context = createContext<DisclosureState | null>(null);

export function ConversationDisclosure({ children }: { children: ReactNode }) {
  const [values, setValues] = useState<ReadonlyMap<string, boolean>>(new Map());
  const setOpen = useCallback((id: string, open: boolean) => {
    setValues((previous) => {
      if (previous.get(id) === open) return previous;
      const next = new Map(previous);
      next.set(id, open);
      return next;
    });
  }, []);
  const value = useMemo(() => ({ values, setOpen }), [values, setOpen]);
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

// 展开状态属于会话，历史虚拟化卸载行或流式增量不能替用户收起内容。
export function useConversationDisclosure(id: string, defaultOpen = false) {
  const context = useContext(Context);
  const [localOpen, setLocalOpen] = useState(defaultOpen);
  const setOpen = useCallback(
    (open: boolean) => {
      if (context) context.setOpen(id, open);
      else setLocalOpen(open);
    },
    [context, id],
  );
  return [
    context ? (context.values.get(id) ?? defaultOpen) : localOpen,
    setOpen,
  ] as const;
}

export function useConversationHasExpanded(keys: readonly string[]) {
  const context = useContext(Context);
  return keys.some((key) => context?.values.get(key) === true);
}
