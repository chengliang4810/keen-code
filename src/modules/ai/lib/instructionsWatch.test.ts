import { describe, expect, it, vi } from "vitest";
import { watchGlobalInstructions } from "@/modules/ai/lib/instructionsWatch";
import type { InstructionsWatch } from "@/modules/ai/lib/instructions";

const subscription: InstructionsWatch = {
  path: "C:/Users/test/.rcode/AGENTS.md",
  directories: ["C:/Users/test", "C:/Users/test/.rcode"],
};
async function settle() {
  for (let i = 0; i < 12; i++) await Promise.resolve();
}
function setup() {
  let emit!: (paths: string[]) => void;
  const stopListening = vi.fn();
  const port = {
    listen: vi.fn(async (onChange: (paths: string[]) => void) => {
      emit = onChange;
      return stopListening;
    }),
    watch: vi.fn(async (_rebind: boolean) => subscription),
    unwatch: vi.fn(async (_directories: string[]) => {}),
  };
  const changed = vi.fn();
  const error = vi.fn();
  const stop = watchGlobalInstructions(changed, error, port, true);
  return {
    port,
    changed,
    error,
    stop,
    stopListening,
    emit: (paths: string[]) => emit(paths),
  };
}

describe("global instruction file watching", () => {
  it("subscribes before watching and filters unrelated files and nested storage", async () => {
    const { port, changed, error, emit, stop } = setup();
    await settle();
    expect(port.listen.mock.invocationCallOrder[0]).toBeLessThan(
      port.watch.mock.invocationCallOrder[0],
    );
    expect(changed).toHaveBeenCalledOnce();
    emit([
      "C:/Users/test/.rcode/credentials/secrets.json",
      "C:/Users/test/other.md",
    ]);
    expect(changed).toHaveBeenCalledOnce();
    emit(["c:\\users\\test\\.rcode\\agents.md"]);
    expect(changed).toHaveBeenCalledTimes(2);
    expect(error).not.toHaveBeenCalled();
    stop();
  });

  it("rebinds after parent directory replacement and balances watcher references", async () => {
    const { port, changed, emit, stop, stopListening } = setup();
    await settle();
    emit(["C:/Users/test/.rcode"]);
    await settle();
    expect(port.watch).toHaveBeenLastCalledWith(true);
    expect(changed).toHaveBeenCalledTimes(2);
    expect(port.unwatch).toHaveBeenCalledExactlyOnceWith(
      subscription.directories,
    );
    stop();
    stop();
    await settle();
    expect(stopListening).toHaveBeenCalledOnce();
    expect(port.unwatch).toHaveBeenCalledTimes(2);
    emit([subscription.path]);
    expect(changed).toHaveBeenCalledTimes(2);
  });

  it("releases a subscription that finishes after its settings section closes", async () => {
    const { port, changed, stop, stopListening } = setup();
    let finish!: (value: InstructionsWatch) => void;
    port.watch.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    await settle();
    stop();
    finish(subscription);
    await settle();
    expect(changed).not.toHaveBeenCalled();
    expect(stopListening).toHaveBeenCalledOnce();
    expect(port.unwatch).toHaveBeenCalledExactlyOnceWith(
      subscription.directories,
    );
  });

  it("reports watch failures without pretending synchronization succeeded", async () => {
    const { port, error, changed, stop } = setup();
    port.watch.mockRejectedValueOnce(new Error("watch failed"));
    await settle();
    expect(error).toHaveBeenCalledOnce();
    expect(changed).not.toHaveBeenCalled();
    stop();
    expect(port.unwatch).not.toHaveBeenCalled();
  });
});
