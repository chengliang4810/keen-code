import { describe, expect, it, vi } from "vitest";
import type { InstructionFile } from "@/modules/ai/lib/instructions";
import { createInstructionEditor } from "@/settings/lib/instructionEditor";

const file = (content: string, exists = true): InstructionFile => ({
  path: "C:/Users/test/.rcode/AGENTS.md",
  content,
  exists,
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function setup(initial = file("original")) {
  let disk = initial;
  const read = vi.fn(async () => disk);
  const save = vi.fn(async (content: string, expected: string | null) => {
    if (expected !== (disk.exists ? disk.content : null))
      throw new Error("conflict");
    disk = file(content);
    return disk;
  });
  const editor = createInstructionEditor({ read, save });
  return {
    editor,
    read,
    save,
    update: (value: InstructionFile) => {
      disk = value;
    },
  };
}

describe("global instruction editor", () => {
  it("tracks external replacement, deletion and recreation without publishing unchanged files", async () => {
    const { editor, update } = setup();
    const changed = vi.fn();
    editor.subscribe(changed);
    await editor.refresh();
    const initial = editor.getSnapshot();
    await editor.refresh();
    expect(editor.getSnapshot()).toBe(initial);
    expect(changed).toHaveBeenCalledTimes(1);
    for (const disk of [file("external"), file("", false), file("recreated")]) {
      update(disk);
      await editor.refresh();
      expect(editor.getSnapshot()).toMatchObject({
        file: disk,
        draft: disk.content,
        incoming: null,
      });
    }
  });

  it("preserves a dirty draft and keeps the latest disk version available until an explicit choice", async () => {
    const { editor, update, save } = setup();
    await editor.refresh();
    editor.edit("my draft");
    update(file("external"));
    await editor.refresh();
    expect(editor.getSnapshot()).toMatchObject({
      draft: "my draft",
      file: file("original"),
      incoming: file("external"),
    });
    update(file("latest external"));
    await editor.refresh();
    expect(save).not.toHaveBeenCalled();
    editor.discard();
    expect(editor.getSnapshot()).toMatchObject({
      draft: "latest external",
      incoming: null,
      file: file("latest external"),
    });
  });

  it("clears conflicts when disk reverts or converges on the current draft", async () => {
    const { editor, update } = setup();
    await editor.refresh();
    editor.edit("local");
    update(file("external"));
    await editor.refresh();
    update(file("original"));
    await editor.refresh();
    expect(editor.getSnapshot()).toMatchObject({
      draft: "local",
      incoming: null,
    });
    update(file("local"));
    await editor.refresh();
    expect(editor.getSnapshot()).toMatchObject({
      file: file("local"),
      draft: "local",
      incoming: null,
    });
  });

  it("coalesces event bursts and preserves edits made while a read is in flight", async () => {
    const { editor, read } = setup();
    await editor.refresh();
    const pending = deferred<InstructionFile>();
    read.mockImplementationOnce(() => pending.promise);
    const first = editor.refresh();
    editor.edit("local");
    for (let i = 0; i < 20; i++) void editor.refresh();
    pending.resolve(file("external"));
    read.mockResolvedValueOnce(file("latest"));
    await first;
    expect(read).toHaveBeenCalledTimes(3);
    expect(editor.getSnapshot()).toMatchObject({
      draft: "local",
      incoming: file("latest"),
    });
  });

  it("checks disk before saving and requires explicit overwrite of a conflict", async () => {
    const { editor, update, save } = setup();
    await editor.refresh();
    editor.edit("local");
    update(file("external"));
    await editor.save();
    expect(save).not.toHaveBeenCalled();
    expect(editor.getSnapshot().incoming).toEqual(file("external"));
    await editor.save(true);
    expect(save).toHaveBeenCalledExactlyOnceWith("local", "external");
    expect(editor.getSnapshot()).toMatchObject({
      file: file("local"),
      draft: "local",
      incoming: null,
      saving: false,
    });
  });

  it("does not overwrite a newer disk version after the user chooses overwrite", async () => {
    const { editor, update, save } = setup();
    await editor.refresh();
    editor.edit("local");
    update(file("external"));
    await editor.refresh();
    update(file("newer"));
    await editor.save(true);
    expect(save).not.toHaveBeenCalled();
    expect(editor.getSnapshot()).toMatchObject({
      draft: "local",
      incoming: file("newer"),
    });
  });

  it("can explicitly recreate a deleted file using a missing-file snapshot", async () => {
    const { editor, update, save } = setup();
    await editor.refresh();
    editor.edit("local");
    update(file("", false));
    await editor.refresh();
    await editor.save(true);
    expect(save).toHaveBeenCalledExactlyOnceWith("local", null);
  });

  it("recovers a native write conflict by reading the latest file without losing the draft", async () => {
    const { editor, update, save } = setup();
    await editor.refresh();
    editor.edit("local");
    save.mockImplementationOnce(async () => {
      update(file("raced"));
      throw new Error(
        "Global AGENTS.md changed on disk. Reload it before saving.",
      );
    });
    await editor.save();
    expect(editor.getSnapshot()).toMatchObject({
      draft: "local",
      incoming: file("raced"),
      saving: false,
      error: "",
    });
  });

  it("keeps a failed save visible and preserves the draft until retry succeeds", async () => {
    const { editor, read, save } = setup();
    await editor.refresh();
    editor.edit("local");
    save.mockRejectedValueOnce(new Error("access denied"));
    await editor.save();
    expect(editor.getSnapshot()).toMatchObject({
      draft: "local",
      saving: false,
      error: "Error: access denied",
    });
    read.mockRejectedValueOnce(new Error("read failed"));
    await editor.refresh();
    expect(editor.getSnapshot().draft).toBe("local");
    await editor.save();
    expect(editor.getSnapshot()).toMatchObject({
      file: file("local"),
      error: "",
    });
  });

  it("serializes save and refresh and rejects duplicate saves", async () => {
    const { editor, save, update, read } = setup();
    await editor.refresh();
    editor.edit("local");
    const pending = deferred<InstructionFile>();
    save.mockImplementationOnce(() => pending.promise);
    const writing = editor.save();
    await vi.waitFor(() => expect(save).toHaveBeenCalledOnce());
    editor.edit("ignored during save");
    void editor.refresh();
    void editor.refresh();
    await editor.save();
    expect(read).toHaveBeenCalledTimes(2);
    update(file("after save"));
    pending.resolve(file("local"));
    await writing;
    await editor.refresh();
    expect(save).toHaveBeenCalledOnce();
    expect(editor.getSnapshot()).toMatchObject({
      draft: "after save",
      incoming: null,
      saving: false,
    });
  });
});
