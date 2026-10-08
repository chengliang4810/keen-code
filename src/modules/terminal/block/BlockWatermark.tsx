import { useTranslation } from "@/modules/i18n";
import { cn } from "@/lib/utils";
import { useEffect, useState, useSyncExternalStore } from "react";
import {
  blockWatermarkState,
  type WatermarkState,
} from "../lib/terminalSessionApi";

type Props = {
  leafId: number;
  subscribe: (cb: () => void) => () => void;
};

const NOOP_SUBSCRIBE = () => () => {};
const DEAD = (): WatermarkState => "dead";

// First-run hints over an untouched block terminal. Once the leaf runs a
// command the component unmounts for good and drops its subscription.
export function BlockWatermark({ leafId, subscribe }: Props) {
  const tr = useTranslation();
  const [gone, setGone] = useState(false);
  const state = useSyncExternalStore(
    gone ? NOOP_SUBSCRIBE : subscribe,
    gone ? DEAD : () => blockWatermarkState(leafId),
  );

  useEffect(() => {
    if (gone || state !== "dead") return;
    const t = setTimeout(() => setGone(true), 600);
    return () => clearTimeout(t);
  }, [state, gone]);

  if (gone) return null;

  return (
    <div
      aria-hidden
      className={cn(
        "pointer-events-none absolute inset-0 z-[5] flex select-none flex-col items-center justify-center gap-8",
        "transition-[opacity,transform] duration-500 ease-out",
        state === "visible"
          ? "translate-y-0 opacity-100"
          : "translate-y-2 opacity-0",
      )}
    >
      <img
        src="/logo.png"
        alt=""
        draggable={false}
        className="size-24 rounded-3xl shadow-lg shadow-black/25"
      />
      <div className="grid grid-cols-[auto_auto] items-center gap-x-12 gap-y-3 text-ui-base">
        <Hint label={tr("Browse your command history")} keys="↑" />
        <Hint label={tr("Autocomplete paths and commands")} keys="Tab" />
      </div>
    </div>
  );
}

function Hint(props: {
  label: string;
  keys: string;
}) {
  return (
    <>
      <span className="justify-self-start text-muted-foreground/60">
        {props.label}
      </span>
      <span className="flex items-center gap-1 justify-self-end">
        <Key>{props.keys}</Key>
      </span>
    </>
  );
}

function Key({ children }: { children: React.ReactNode }) {
  return (
    <kbd className="inline-flex h-[22px] min-w-[22px] items-center justify-center rounded-md border border-border/45 bg-muted/30 px-1.5 font-sans text-ui-xs font-medium text-muted-foreground/80">
      {children}
    </kbd>
  );
}
