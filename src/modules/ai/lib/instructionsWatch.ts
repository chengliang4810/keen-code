import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { IS_WINDOWS } from "@/lib/platform";
import type { InstructionsWatch } from "@/modules/ai/lib/instructions";

type Stop = () => void;
type Port = {
  listen: (onChange: (paths: string[]) => void) => Promise<Stop>;
  watch: (rebind: boolean) => Promise<InstructionsWatch>;
  unwatch: (directories: string[]) => Promise<unknown>;
};

const nativePort: Port = {
  listen: (onChange) =>
    getCurrentWebviewWindow().listen<{ paths: string[] }>(
      "fs:changed",
      (event) => onChange(event.payload.paths),
    ),
  watch: (rebind) => invoke("agent_instructions_watch", { rebind }),
  unwatch: (directories) =>
    invoke("fs_watch_remove", {
      paths: directories,
      workspace: { kind: "local" },
    }),
};

export function watchGlobalInstructions(
  onChange: () => void,
  onError: (error: unknown) => void,
  port = nativePort,
  caseInsensitive = IS_WINDOWS,
): Stop {
  let disposed = false;
  let current: InstructionsWatch | null = null;
  let unlisten: Stop | undefined;
  let updating = false;
  let pending = false;
  const normalize = (path: string) => {
    const canonical = path.replace(/\\/g, "/").replace(/\/$/, "");
    return caseInsensitive ? canonical.toLowerCase() : canonical;
  };
  const report = (error: unknown) => {
    if (!disposed) onError(error);
  };
  const release = (watch: InstructionsWatch) =>
    port.unwatch(watch.directories).catch(report);

  const update = async (rebind: boolean) => {
    if (disposed) return;
    if (updating) {
      pending = true;
      return;
    }
    updating = true;
    try {
      const next = await port.watch(rebind);
      if (disposed) {
        await release(next);
        return;
      }
      const previous = current;
      current = next;
      if (previous) await release(previous);
      if (!disposed) onChange();
    } catch (error) {
      report(error);
    } finally {
      updating = false;
      if (pending && !disposed) {
        pending = false;
        void update(true);
      }
    }
  };

  void port
    .listen((paths) => {
      if (disposed || !current) return;
      const file = normalize(current.path);
      const parent = file.slice(0, file.lastIndexOf("/"));
      const changed = paths.map(normalize);
      if (changed.includes(parent)) void update(true);
      else if (changed.includes(file)) onChange();
    })
    .then((stop) => {
      if (disposed) {
        stop();
        return;
      }
      unlisten = stop;
      void update(false);
    })
    .catch(report);

  return () => {
    if (disposed) return;
    disposed = true;
    unlisten?.();
    if (current) void release(current);
    current = null;
  };
}
