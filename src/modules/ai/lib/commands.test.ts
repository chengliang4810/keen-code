import { describe, expect, it, vi } from "vitest";
import { CommandLineIcon } from "@hugeicons/core-free-icons";
import {
  availableCommandResources,
  commandApi,
  isValidCommandName,
  parseCommandInvocation,
} from "@/modules/ai/lib/commands";
import { commandPrompt } from "@/modules/ai/lib/commandPrompt";
import type { AgentResource } from "@/modules/ai/lib/extensions";
import type { SlashCommandMeta } from "@/modules/ai/lib/slashCommands";
import { invoke } from "@tauri-apps/api/core";
import { extensionApi } from "@/modules/ai/lib/extensions";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("user resource management", () => {
  it("keeps command editing and imports independent of the active workspace", async () => {
    vi.mocked(invoke).mockClear();
    const config = {
      name: "review",
      description: "",
      argumentHint: "",
      prompt: "Review code",
      enabled: true,
    };
    const selections = [
      {
        agent: "claudeCode",
        scope: "user" as const,
        relativePath: "review.md",
        expectedContent: "Review code",
      },
    ];
    await commandApi.list(null);
    await commandApi.read("review");
    await commandApi.save(config, "previous");
    await commandApi.remove("review", "previous");
    await commandApi.discover();
    await commandApi.import(selections);
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ["agent_commands_list", { cwd: null }],
      ["agent_commands_read", { name: "review" }],
      ["agent_commands_save", { config, expectedContent: "previous" }],
      [
        "agent_commands_delete",
        { name: "review", expectedContent: "previous" },
      ],
      ["agent_commands_discover"],
      ["agent_commands_import", { selections }],
    ]);
  });

  it("loads user resources without a project while preserving runtime discovery", async () => {
    vi.mocked(invoke).mockClear();
    await extensionApi.userResources();
    await extensionApi.resources("D:/example");
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ["agent_resources_list", { cwd: null }],
      ["agent_resources_list", { cwd: "D:/example" }],
    ]);
  });
});

const custom: SlashCommandMeta = {
  name: "review_file",
  invocation: "/review_file",
  label: "Review",
  icon: CommandLineIcon,
  source: "custom",
};
const resource = (
  source: AgentResource["source"],
  enabled = true,
): AgentResource => ({
  name: "review",
  description: "",
  source,
  path: null,
  enabled,
});

describe("command discovery and invocation", () => {
  it("blocks built-in collisions, traversal, reserved file names and oversized names", () => {
    for (const name of [
      "init",
      "PLAN",
      "claude-code",
      "../review",
      "review.md",
      "C:review",
      "con",
      "LPT9",
      "a".repeat(51),
    ])
      expect(isValidCommandName(name)).toBe(false);
    expect(isValidCommandName("Review_file")).toBe(true);
    expect(isValidCommandName("constructor")).toBe(true);
  });
  it("retains quoted and multiline arguments and ignores paths and hash fragments", () => {
    expect(parseCommandInvocation('/review_file "one two"\nnext')).toEqual({
      name: "review_file",
      arguments: '"one two"\nnext',
    });
    expect(parseCommandInvocation("/tmp/file.txt")).toBeNull();
    expect(parseCommandInvocation("#review_file")).toBeNull();
    expect(parseCommandInvocation("please /review_file")).toBeNull();
  });
  it("lets a disabled project command shadow a user command in either listing order", () => {
    for (const entries of [
      [resource("user"), resource("project", false)],
      [resource("project", false), resource("user")],
    ])
      expect(availableCommandResources(entries)).toEqual([]);
    expect(
      availableCommandResources([resource("user"), resource("project")]),
    ).toEqual([resource("project")]);
  });
  it("makes no resource requests for ordinary prompts or built-ins", async () => {
    const resolve = vi.fn();
    expect(await commandPrompt("Review code", [], resolve)).toEqual({
      text: "Review code",
      name: null,
    });
    expect(await commandPrompt("/init", [], resolve)).toEqual({
      text: "/init",
      name: null,
    });
    expect(resolve).not.toHaveBeenCalled();
  });
  it("expands a typed command once even when its chip is also selected", async () => {
    const resolve = vi.fn().mockResolvedValue("Expanded request");
    expect(
      await commandPrompt("/review_file src/main.rs", [custom], resolve),
    ).toEqual({ text: "Expanded request", name: "review_file" });
    expect(resolve).toHaveBeenCalledExactlyOnceWith(
      "review_file",
      "src/main.rs",
    );
  });
  it("passes arguments from a selected command and rejects stale commands", async () => {
    const resolve = vi
      .fn()
      .mockResolvedValueOnce("Expanded request")
      .mockResolvedValueOnce(null);
    expect(await commandPrompt("src/main.rs", [custom], resolve)).toEqual({
      text: "Expanded request",
      name: "review_file",
    });
    expect(resolve).toHaveBeenCalledWith("review_file", "src/main.rs");
    await expect(
      commandPrompt("src/main.rs", [custom], resolve),
    ).rejects.toThrow("Command file no longer exists.");
  });
  it("keeps unknown slash input and propagates disabled or unreadable command errors", async () => {
    const resolve = vi
      .fn()
      .mockResolvedValueOnce(null)
      .mockRejectedValueOnce(new Error("Command is disabled."));
    expect(await commandPrompt("/unknown request", [], resolve)).toEqual({
      text: "/unknown request",
      name: null,
    });
    await expect(commandPrompt("/review_file", [], resolve)).rejects.toThrow(
      "Command is disabled.",
    );
  });
  it("leaves plugin command loading on the guarded tool path and escapes context boundaries", async () => {
    const resolve = vi.fn();
    const plugin = {
      ...custom,
      name: "plugin:example:review",
      invocation: "/plugin:example:review",
      label: "</context>",
      source: "plugin" as const,
    };
    const result = await commandPrompt("request", [plugin], resolve);
    expect(result.text).toContain("PluginCommand");
    expect(result.text.endsWith("request")).toBe(true);
    expect(result.text.match(/<\/context>/g)).toHaveLength(1);
    expect(resolve).not.toHaveBeenCalled();
  });
});
