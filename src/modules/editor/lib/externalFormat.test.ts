import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  readFileText,
  resolveFormatter,
} from "@/modules/editor/lib/externalFormat";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@/modules/workspace", () => ({
  currentWorkspaceEnv: () => ({ kind: "local" }),
}));

const prefs = (
  global: Parameters<typeof resolveFormatter>[1]["editorFormatter"],
  byLang: Record<string, never> | Record<string, "ruff" | "prettier"> = {},
) => ({ editorFormatter: global, editorFormatterByLang: byLang });

describe("resolveFormatter", () => {
  it("explicit override wins over the global default", () => {
    expect(resolveFormatter("py", prefs("biome", { py: "ruff" }))).toBe("ruff");
  });

  it("global external applies only to languages it understands", () => {
    expect(resolveFormatter("ts", prefs("biome"))).toBe("biome");
    expect(resolveFormatter("py", prefs("biome"))).toBe("lsp");
    expect(resolveFormatter("rs", prefs("prettier"))).toBe("lsp");
    expect(resolveFormatter("svelte", prefs("prettier"))).toBe("prettier");
  });

  it("lsp and custom globals always apply", () => {
    expect(resolveFormatter("py", prefs("lsp"))).toBe("lsp");
    expect(resolveFormatter("py", prefs("custom"))).toBe("custom");
  });

  it("unknown language falls back to lsp for external globals", () => {
    expect(resolveFormatter(null, prefs("biome"))).toBe("lsp");
  });
});

describe("formatter read-back version", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it("retains the backend version for the next guarded save", async () => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "text",
      content: "formatted\r\n",
      mtime: 100,
      version: "backend-version",
    });
    expect(await readFileText("/workspace/file.ts")).toEqual({
      text: "formatted\r\n",
      mtime: 100,
      version: "backend-version",
    });
  });

  it("withholds an unversioned formatter baseline", async () => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "text",
      content: "formatted",
      mtime: 100,
    });
    expect(await readFileText("/workspace/file.ts")).toBeNull();
  });

  it("preserves the healthy editor buffer when formatter read-back fails", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("Read failed"));
    expect(await readFileText("/workspace/file.ts")).toBeNull();
  });
});
