import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import * as api from "@/lib/api";
import { IconClose, IconPlus, IconTerminal } from "@/components/icons";
import { Tip } from "@/components/ui/tooltip";
import { createT, type Locale } from "@/i18n";

type TerminalTab = {
  id: string;
  title: string;
  exited: boolean;
};

type TerminalRuntime = {
  terminal: Terminal;
  fit: FitAddon;
  host: HTMLDivElement | null;
  resizeObserver: ResizeObserver | null;
};

type TerminalOutput = { id: string; data: number[] };
type TerminalExited = { id: string };

export function TerminalPanel({
  projectPath,
  locale,
  active,
}: {
  projectPath: string | null;
  locale: Locale;
  active: boolean;
}) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const runtimes = useRef(new Map<string, TerminalRuntime>());
  const sequence = useRef(0);

  const fitRuntime = useCallback((id: string) => {
    const runtime = runtimes.current.get(id);
    if (!runtime?.host || runtime.host.offsetWidth === 0) return;
    runtime.fit.fit();
    void api
      .terminalResize(id, runtime.terminal.cols, runtime.terminal.rows)
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!api.isTauri()) return;
    let disposed = false;
    const unlisteners: UnlistenFn[] = [];
    void Promise.all([
      listen<TerminalOutput>("terminal://output", ({ payload }) => {
        runtimes.current.get(payload.id)?.terminal.write(
          Uint8Array.from(payload.data),
        );
      }),
      listen<TerminalExited>("terminal://exited", ({ payload }) => {
        setTabs((current) =>
          current.map((tab) =>
            tab.id === payload.id ? { ...tab, exited: true } : tab,
          ),
        );
      }),
    ]).then((items) => {
      if (disposed) items.forEach((unlisten) => unlisten());
      else unlisteners.push(...items);
    });
    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    if (active && activeId) requestAnimationFrame(() => fitRuntime(activeId));
  }, [active, activeId, fitRuntime]);

  const mountTerminal = useCallback(
    (id: string, host: HTMLDivElement | null) => {
      const runtime = runtimes.current.get(id);
      if (!runtime || runtime.host === host) return;
      runtime.resizeObserver?.disconnect();
      runtime.host = host;
      if (!host) return;
      runtime.terminal.open(host);
      const observer = new ResizeObserver(() => fitRuntime(id));
      observer.observe(host);
      runtime.resizeObserver = observer;
      requestAnimationFrame(() => fitRuntime(id));
    },
    [fitRuntime],
  );

  const createTerminal = useCallback(async () => {
    if (!projectPath || !api.isTauri()) return;
    const id = `terminal-${Date.now()}-${++sequence.current}`;
    const terminal = new Terminal({
      cursorBlink: true,
      convertEol: false,
      fontFamily:
        'ui-monospace, "SFMono-Regular", Menlo, Monaco, Consolas, monospace',
      fontSize: 12,
      lineHeight: 1.2,
      scrollback: 5000,
      theme: {
        background: "#0d0d0d",
        foreground: "#d8d8d8",
        cursor: "#d8d8d8",
        selectionBackground: "#4d69a855",
      },
    });
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.onData((data) => {
      void api
        .terminalWrite(id, Array.from(new TextEncoder().encode(data)))
        .catch((reason) => setError(String(reason)));
    });
    runtimes.current.set(id, {
      terminal,
      fit,
      host: null,
      resizeObserver: null,
    });
    setTabs((current) => [
      ...current,
      { id, title: tr("terminal.tabName", { number: current.length + 1 }), exited: false },
    ]);
    setActiveId(id);
    setError(null);
    try {
      await api.terminalCreate(id, projectPath, 100, 30);
      requestAnimationFrame(() => fitRuntime(id));
    } catch (reason) {
      terminal.writeln(
        `\r\n\x1b[31m${tr("terminal.startFailed", { error: String(reason) })}\x1b[0m`,
      );
      setTabs((current) =>
        current.map((tab) => (tab.id === id ? { ...tab, exited: true } : tab)),
      );
    }
  }, [fitRuntime, projectPath, tr]);

  const closeTerminal = useCallback((id: string) => {
    void api.terminalClose(id).catch(() => {});
    const runtime = runtimes.current.get(id);
    runtime?.resizeObserver?.disconnect();
    runtime?.terminal.dispose();
    runtimes.current.delete(id);
    setTabs((current) => {
      const index = current.findIndex((tab) => tab.id === id);
      const next = current.filter((tab) => tab.id !== id);
      setActiveId((selected) =>
        selected === id
          ? (next[Math.min(index, next.length - 1)]?.id ?? null)
          : selected,
      );
      return next;
    });
  }, []);

  useEffect(
    () => () => {
      for (const [id, runtime] of runtimes.current) {
        void api.terminalClose(id).catch(() => {});
        runtime.resizeObserver?.disconnect();
        runtime.terminal.dispose();
      }
      runtimes.current.clear();
    },
    [],
  );

  return (
    <section className={"terminal-panel" + (active ? " is-active" : "")}>
      <div className="terminal-tabs" role="tablist" aria-label={tr("terminal.tabsAria")}>
        <div className="terminal-tabs__scroll">
          {tabs.map((tab) => (
            <button
              key={tab.id}
              type="button"
              role="tab"
              aria-selected={tab.id === activeId}
              className={"terminal-tab" + (tab.id === activeId ? " is-active" : "")}
              onClick={() => setActiveId(tab.id)}
            >
              <IconTerminal size={13} />
              <span>
                {tab.title}
                {tab.exited ? `（${tr("terminal.exited")}）` : ""}
              </span>
              <span
                className="terminal-tab__close"
                role="button"
                tabIndex={0}
                onClick={(event) => {
                  event.stopPropagation();
                  closeTerminal(tab.id);
                }}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.stopPropagation();
                    closeTerminal(tab.id);
                  }
                }}
              >
                <IconClose size={11} />
              </span>
            </button>
          ))}
        </div>
        <Tip label={tr("terminal.new")}>
          <button
            type="button"
            className="terminal-tabs__add"
            disabled={!projectPath}
            onClick={() => void createTerminal()}
            aria-label={tr("terminal.new")}
          >
            <IconPlus size={14} />
          </button>
        </Tip>
      </div>
      {error ? <div className="terminal-panel__error">{error}</div> : null}
      <div className="terminal-panel__body">
        {tabs.length === 0 ? (
          <div className="terminal-panel__empty">
            <IconTerminal size={24} />
            <span>
              {projectPath ? tr("terminal.empty") : tr("terminal.needProject")}
            </span>
          </div>
        ) : null}
        {tabs.map((tab) => (
          <div
            key={tab.id}
            className={
              "terminal-instance" + (tab.id === activeId ? " is-active" : "")
            }
            ref={(host) => mountTerminal(tab.id, host)}
          />
        ))}
      </div>
    </section>
  );
}
