import { useCallback, useEffect, useMemo, useRef, useState, type SetStateAction } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import * as api from "@/lib/api";
import { IconTerminal } from "@/components/icons";
import { createT, type Locale } from "@/i18n";
import { localizeUiError } from "@/lib/session";

export type TerminalTab = {
  id: string;
  title: string;
  exited: boolean;
  sessionKey: string;
};

type TerminalRuntime = {
  terminal: Terminal;
  fit: FitAddon;
  host: HTMLDivElement | null;
  resizeObserver: ResizeObserver | null;
};

type TerminalOutput = { id: string; data: number[] };
type TerminalExited = { id: string };

const DEFAULT_TERMINAL_FONT_FAMILY =
  'ui-monospace, "SFMono-Regular", Menlo, Monaco, Consolas, monospace';

export function TerminalPanel({
  sessionKey,
  projectPath,
  locale,
  fontFamily = DEFAULT_TERMINAL_FONT_FAMILY,
  active,
  activeTabId,
  createRequest = 0,
  closeRequests = [],
  onTabsChange,
}: {
  sessionKey: string;
  projectPath: string | null;
  locale: Locale;
  fontFamily?: string;
  active: boolean;
  activeTabId?: string | null;
  createRequest?: number;
  closeRequests?: string[];
  onTabsChange?: (tabs: TerminalTab[], activeId: string | null) => void;
}) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const [activeIds, setActiveIds] = useState<Record<string, string | null>>({});
  const activeId = activeIds[sessionKey] ?? null;
  const setActiveId = useCallback((next: SetStateAction<string | null>) => {
    setActiveIds((current) => ({
      ...current,
      [sessionKey]: typeof next === "function" ? next(current[sessionKey] ?? null) : next,
    }));
  }, [sessionKey]);
  const visibleTabs = useMemo(
    () => tabs.filter((tab) => tab.sessionKey === sessionKey),
    [sessionKey, tabs],
  );
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

  useEffect(() => {
    for (const [id, runtime] of runtimes.current) {
      runtime.terminal.options.fontFamily = fontFamily;
      requestAnimationFrame(() => fitRuntime(id));
    }
  }, [fontFamily, fitRuntime]);

  useEffect(() => {
    if (activeTabId && activeTabId !== activeId && visibleTabs.some((tab) => tab.id === activeTabId)) {
      setActiveId(activeTabId);
    }
  }, [activeId, activeTabId, visibleTabs, setActiveId]);

  useEffect(() => {
    onTabsChange?.(visibleTabs, activeId);
  }, [activeId, onTabsChange, visibleTabs]);

  const mountTerminal = useCallback(
    (id: string, host: HTMLDivElement | null) => {
      const runtime = runtimes.current.get(id);
      if (!runtime || runtime.host === host) return;
      runtime.resizeObserver?.disconnect();
      runtime.host = host;
      if (!host) return;
      runtime.terminal.open(host);
      let frame = 0;
      const observer = new ResizeObserver(() => {
        if (!frame) {
          frame = requestAnimationFrame(() => {
            frame = 0;
            fitRuntime(id);
          });
        }
      });
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
      fontFamily,
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
        .catch((reason) => setError(localizeUiError(reason, locale)));
    });
    runtimes.current.set(id, {
      terminal,
      fit,
      host: null,
      resizeObserver: null,
    });
    setTabs((current) => [
      ...current,
      { id, title: tr("terminal.tabName", { number: visibleTabs.length + 1 }), exited: false, sessionKey },
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
  }, [fitRuntime, fontFamily, projectPath, sessionKey, tr, visibleTabs.length]);

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

  const handledCreateRequests = useRef<Record<string, number>>({});
  if (!(sessionKey in handledCreateRequests.current)) {
    handledCreateRequests.current[sessionKey] = createRequest;
  }
  useEffect(() => {
    if (createRequest === handledCreateRequests.current[sessionKey]) return;
    handledCreateRequests.current[sessionKey] = createRequest;
    void createTerminal();
  }, [createRequest, createTerminal, sessionKey]);

  const handledCloseRequests = useRef(new Set<string>());
  useEffect(() => {
    closeRequests.forEach((id) => {
      if (handledCloseRequests.current.has(id)) return;
      handledCloseRequests.current.add(id);
      closeTerminal(id);
    });
  }, [closeRequests, closeTerminal]);

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
      {error ? <div className="terminal-panel__error">{error}</div> : null}
      <div className="terminal-panel__body">
        {visibleTabs.length === 0 ? (
          <div className="terminal-panel__empty">
            <IconTerminal size={24} />
            <span>
              {projectPath ? tr("terminal.empty") : tr("terminal.needProject")}
            </span>
          </div>
        ) : null}
        {visibleTabs.map((tab) => (
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
