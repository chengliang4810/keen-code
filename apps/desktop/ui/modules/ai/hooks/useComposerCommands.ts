import { useCallback, useEffect, useRef, useState } from "react";
import {
  availableCommandResources,
  commandApi,
  resourceCommand,
} from "@/modules/ai/lib/commands";
import { ensureDefaultChatDirectory } from "@/modules/ai/lib/defaultChatDirectory";
import { getTaskWorkspace, useChatStore } from "@/modules/ai/store/chatStore";
import type { SlashCommandMeta } from "@/modules/ai/lib/slashCommands";
import { useWorkspaceEnvStore } from "@/modules/workspace";

export function useComposerCommands(enabled: boolean) {
  const id = useChatStore((s) => s.activeSessionId);
  const sessions = useChatStore((s) => s.sessions);
  const draft = useChatStore((s) => s.draftSession);
  const env = useWorkspaceEnvStore((s) => s.env);
  const session =
    sessions.find((session) => session.id === id) ??
    (draft?.id === id ? draft : undefined);
  const projectless = session?.projectless === true;
  const root = id ? getTaskWorkspace(id) : null;
  const [snapshot, setSnapshot] = useState<{
    id: string | null;
    root: string | null;
    commands: SlashCommandMeta[];
  } | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const generation = useRef(0);
  const reload = useCallback(async () => {
    const current = ++generation.current;
    if (!enabled || env.kind !== "local") {
      setLoading(false);
      setSnapshot(null);
      return;
    }
    setLoading(true);
    setError("");
    setSnapshot(null);
    try {
      const directory = projectless ? await ensureDefaultChatDirectory() : root;
      if (!directory || current !== generation.current) return;
      const catalog = await commandApi.list(directory);
      const resources = [
        ...catalog.commands.map((entry) => ({
          name: entry.name,
          description: entry.description,
          path: entry.path,
          enabled: entry.enabled,
          source: entry.scope,
        })),
        ...catalog.pluginCommands.map((entry) => ({
          name: entry.name,
          description: entry.description,
          path: entry.path,
          enabled: true,
          source: "plugin" as const,
        })),
      ];
      if (current === generation.current)
        setSnapshot({
          id,
          root,
          commands: availableCommandResources(resources).map(resourceCommand),
        });
    } catch (error) {
      if (current === generation.current) setError(String(error));
    } finally {
      if (current === generation.current) setLoading(false);
    }
  }, [enabled, id, root, projectless, env.kind]);
  useEffect(() => {
    void reload();
    if (!enabled) return;
    const changed = () => {
      void reload();
    };
    window.addEventListener("rcode:commands-changed", changed);
    return () => {
      generation.current += 1;
      window.removeEventListener("rcode:commands-changed", changed);
    };
  }, [enabled, reload]);
  return {
    commands:
      env.kind === "local" && snapshot?.id === id && snapshot.root === root
        ? snapshot.commands
        : [],
    loading,
    error,
    retry: reload,
  };
}
