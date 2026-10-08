import type { InstructionFile } from "@/modules/ai/lib/instructions";

export type InstructionEditorState = {
  file: InstructionFile | null;
  draft: string;
  incoming: InstructionFile | null;
  saving: boolean;
  error: string;
};

function sameFile(a: InstructionFile | null, b: InstructionFile | null) {
  return (
    a === b ||
    (a !== null &&
      b !== null &&
      a.path === b.path &&
      a.content === b.content &&
      a.exists === b.exists)
  );
}

export function receiveInstructionFile(
  state: InstructionEditorState,
  file: InstructionFile,
): InstructionEditorState {
  if (sameFile(state.file, file)) {
    return state.incoming ? { ...state, incoming: null } : state;
  }
  if (
    !state.file ||
    state.draft === state.file.content ||
    state.draft === file.content
  ) {
    return { ...state, file, draft: file.content, incoming: null };
  }
  return sameFile(state.incoming, file) ? state : { ...state, incoming: file };
}

export function createInstructionEditor(port: {
  read: () => Promise<InstructionFile>;
  save: (content: string, expected: string | null) => Promise<InstructionFile>;
}) {
  let state: InstructionEditorState = {
    file: null,
    draft: "",
    incoming: null,
    saving: false,
    error: "",
  };
  const listeners = new Set<() => void>();
  let reading: Promise<void> | null = null;
  let pending = false;
  const publish = (next: InstructionEditorState) => {
    if (next === state) return;
    state = next;
    for (const listener of listeners) listener();
  };
  const setError = (error: unknown) => {
    const value = String(error);
    if (state.error !== value) publish({ ...state, error: value });
  };
  const receive = (file: InstructionFile) => {
    const next = receiveInstructionFile(state, file);
    publish(next.error ? { ...next, error: "" } : next);
  };
  const refresh = (): Promise<void> => {
    pending = true;
    if (reading) return reading;
    if (state.saving) return Promise.resolve();
    reading = (async () => {
      do {
        pending = false;
        try {
          receive(await port.read());
        } catch (error) {
          setError(error);
        }
      } while (pending && !state.saving);
    })().finally(() => {
      reading = null;
    });
    return reading;
  };

  return {
    getSnapshot: () => state,
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    refresh,
    edit: (draft: string) => {
      if (state.saving) return;
      const next = { ...state, draft };
      publish(
        state.incoming ? receiveInstructionFile(next, state.incoming) : next,
      );
    },
    discard: () => {
      if (state.saving || !state.incoming) return;
      publish({
        ...state,
        file: state.incoming,
        draft: state.incoming.content,
        incoming: null,
        error: "",
      });
    },
    save: async (overwrite = false) => {
      if (state.saving || !state.file) return;
      const expected = overwrite ? (state.incoming ?? state.file) : state.file;
      const draft = state.draft;
      publish({ ...state, saving: true, error: "" });
      try {
        await reading;
        const latest = await port.read();
        receive(latest);
        if (!sameFile(expected, latest)) return;
        if (draft === latest.content) return;
        const saved = await port.save(
          draft,
          latest.exists ? latest.content : null,
        );
        publish({
          ...state,
          file: saved,
          draft: saved.content,
          incoming: null,
          error: "",
        });
      } catch (error) {
        setError(error);
        try {
          const latest = await port.read();
          const next = receiveInstructionFile(state, latest);
          publish(next.incoming ? { ...next, error: "" } : next);
        } catch {
          // Keep the failed save and the draft visible until a later refresh succeeds.
        }
      } finally {
        publish({ ...state, saving: false });
        if (pending) void refresh();
      }
    },
  };
}
