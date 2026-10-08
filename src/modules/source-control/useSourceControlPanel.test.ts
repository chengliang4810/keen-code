import { beforeEach, describe, expect, it, vi } from "vitest";
import { useSourceControlPanel } from "@/modules/source-control/useSourceControlPanel";
import { buildConfiguredLanguageModel } from "@/modules/ai/lib/agent";
import type { SourceControlSummary } from "@/modules/source-control/useSourceControl";
import type { CustomEndpoint } from "@/modules/ai/config";

const fixture = vi.hoisted(() => ({
  slots: [] as unknown[],
  cursor: 0,
  generateText: vi.fn(),
  gitDiff: vi.fn(),
  prefs: {} as Record<string, unknown>,
  chat: {} as Record<string, unknown>,
}));
vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useState: (initial: unknown) => {
    const index = fixture.cursor++;
    if (!(index in fixture.slots))
      fixture.slots[index] =
        typeof initial === "function" ? initial() : initial;
    return [
      fixture.slots[index],
      (value: unknown) => {
        fixture.slots[index] =
          typeof value === "function" ? value(fixture.slots[index]) : value;
      },
    ];
  },
  useEffect: (effect: () => unknown) => {
    effect();
  },
  useMemo: (factory: () => unknown) => factory(),
  useCallback: (callback: unknown) => callback,
  useRef: (value: unknown) => ({ current: value }),
}));
vi.mock("@/modules/i18n", () => ({
  useTranslation: () => (value: string) => value,
}));
vi.mock("@/modules/ai/store/chatStore", () => ({
  useChatStore: Object.assign(
    (selector: (state: typeof fixture.chat) => unknown) =>
      selector(fixture.chat),
    { getState: () => fixture.chat },
  ),
}));
vi.mock("@/modules/settings/preferences", () => ({
  usePreferencesStore: Object.assign(
    (selector: (state: typeof fixture.prefs) => unknown) =>
      selector(fixture.prefs),
    { getState: () => fixture.prefs },
  ),
}));
vi.mock("@/modules/ai/lib/native", () => ({
  native: { gitDiff: fixture.gitDiff },
}));
vi.mock("@/modules/ai/tools/tools", () => ({ buildTools: vi.fn() }));
vi.mock("@/modules/ai/lib/agent", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("@/modules/ai/lib/agent")>();
  return {
    ...actual,
    buildConfiguredLanguageModel: vi.fn(actual.buildConfiguredLanguageModel),
  };
});
vi.mock("ai", async (importOriginal) => ({
  ...(await importOriginal<typeof import("ai")>()),
  generateText: fixture.generateText,
}));

const endpoint: CustomEndpoint = {
  id: "test",
  name: "Test provider",
  baseURL: "http://127.0.0.1:18765/v1",
  contextLimit: 64000,
  modelId: "",
  models: [{ id: "model", enabled: true, reasoningLevels: [] }],
};
const modelId = "compat-test/model";
const summary = {
  repo: {
    repoRoot: "D:/fixture",
    branch: "main",
    upstream: null,
    isDetached: false,
  },
  hasRepo: true,
  isLoading: false,
  busyAction: null,
  localError: null,
  lastRemoteError: null,
  status: {
    branch: "main",
    upstream: null,
    ahead: 0,
    behind: 0,
    isDetached: false,
    changedFiles: [
      {
        path: "file.txt",
        indexStatus: "M",
        worktreeStatus: " ",
        staged: true,
        unstaged: false,
        untracked: false,
        originalPath: null,
        statusLabel: "Modified",
      },
    ],
  },
  refresh: vi.fn(),
  applyStatus: vi.fn(),
  runRemoteAction: vi.fn(),
} as unknown as SourceControlSummary;

function SourceControlFixture(current = summary) {
  fixture.cursor = 0;
  return useSourceControlPanel(true, current, vi.fn());
}

beforeEach(() => {
  vi.clearAllMocks();
  fixture.slots = [];
  fixture.cursor = 0;
  fixture.chat = {
    selectedModelId: modelId,
    apiKeys: {},
    customEndpointKeys: { test: "fixture-key" },
    sessions: [],
    draftSession: null,
    activeSessionId: null,
    agentMeta: { status: "idle" },
  };
  fixture.prefs = {
    customEndpoints: structuredClone([endpoint]),
    lmstudioModelId: "",
    mlxModelId: "",
    ollamaModelId: "",
    openaiCompatibleBaseURL: "",
    openaiCompatibleModelId: "",
    openrouterModelId: "",
  };
  fixture.gitDiff.mockResolvedValue({
    diffText: "diff --git a/file.txt b/file.txt\n+test",
  });
  fixture.generateText.mockResolvedValue({ text: "fix: update test fixture" });
});

describe("source control AI configuration", () => {
  it.each(["chat_completions", "responses", "messages"] as const)(
    "generates a commit message through the configured %s endpoint",
    async (protocol) => {
      fixture.prefs.customEndpoints = [{ ...endpoint, protocol }];
      SourceControlFixture();
      await SourceControlFixture().generateCommitMessage();
      expect(SourceControlFixture()).toMatchObject({
        actionError: null,
        commitMessage: "fix: update test fixture",
      });
      expect(buildConfiguredLanguageModel).toHaveBeenCalledWith(
        modelId,
        {},
        expect.objectContaining({
          customEndpoints: [{ ...endpoint, protocol }],
          customEndpointKeys: { test: "fixture-key" },
        }),
      );
      expect(fixture.generateText).toHaveBeenCalledOnce();
    },
  );

  it("freezes model settings and keys before waiting for the staged diff", async () => {
    let finish!: (diff: { diffText: string }) => void;
    fixture.gitDiff.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    SourceControlFixture();
    const pending = SourceControlFixture().generateCommitMessage();
    (fixture.prefs.customEndpoints as CustomEndpoint[])[0].baseURL =
      "http://127.0.0.1:18766/v1";
    (fixture.chat.customEndpointKeys as Record<string, string>).test =
      "changed-key";
    finish({ diffText: "+test" });
    await pending;
    expect(buildConfiguredLanguageModel).toHaveBeenCalledWith(
      modelId,
      {},
      expect.objectContaining({
        customEndpoints: [endpoint],
        customEndpointKeys: { test: "fixture-key" },
      }),
    );
  });

  it.each([
    { customEndpoints: [] },
    {
      customEndpoints: [
        { ...endpoint, models: [{ id: "model", enabled: false }] },
      ],
    },
  ])(
    "rejects a deleted or disabled model before making a provider request",
    async ({ customEndpoints }) => {
      fixture.prefs.customEndpoints = customEndpoints;
      SourceControlFixture();
      await SourceControlFixture().generateCommitMessage();
      expect(SourceControlFixture().actionError).toBe(
        "Connect an AI provider to generate commit messages",
      );
      expect(buildConfiguredLanguageModel).not.toHaveBeenCalled();
      expect(fixture.generateText).not.toHaveBeenCalled();
    },
  );

  it("keeps endpoint keys optional for local models", async () => {
    fixture.chat.customEndpointKeys = {};
    SourceControlFixture();
    await SourceControlFixture().generateCommitMessage();
    expect(SourceControlFixture().actionError).toBeNull();
    expect(fixture.generateText).toHaveBeenCalledOnce();
  });

  it("retains legacy Messages thinking with an output cap above its minimum budget", async () => {
    fixture.prefs.customEndpoints = [
      {
        ...endpoint,
        protocol: "messages",
        models: [
          {
            id: "model",
            reasoningLevels: ["low", "high"],
            maxOutputTokens: 8192,
          },
        ],
      },
    ];
    fixture.chat.sessions = [
      { id: "task", reasoningSelection: { modelId, level: "low" } },
    ];
    fixture.chat.activeSessionId = "task";
    SourceControlFixture();
    await SourceControlFixture().generateCommitMessage();
    expect(SourceControlFixture().actionError).toBeNull();
    expect(buildConfiguredLanguageModel).toHaveBeenCalledWith(
      modelId,
      {},
      expect.objectContaining({ reasoningLevel: "low", maxOutputTokens: 4096 }),
    );
    expect(fixture.generateText).toHaveBeenCalledWith(
      expect.objectContaining({ maxOutputTokens: 4096 }),
    );
  });

  it("requires a new selection after a configured reasoning level is removed", async () => {
    fixture.prefs.customEndpoints = [
      {
        ...endpoint,
        models: [{ id: "model", reasoningLevels: ["low", "high"] }],
      },
    ];
    fixture.chat.sessions = [
      { id: "task", reasoningSelection: { modelId, level: "deleted" } },
    ];
    fixture.chat.activeSessionId = "task";
    SourceControlFixture();
    await SourceControlFixture().generateCommitMessage();
    expect(SourceControlFixture().actionError).toBe(
      "Select a valid reasoning level before sending.",
    );
    expect(fixture.generateText).not.toHaveBeenCalled();
  });
});

describe("source control failed lookup", () => {
  it("exposes the cause and existing refresh action after a failed initial lookup", async () => {
    const failed = {
      ...summary,
      repo: null,
      status: null,
      hasRepo: false,
      localError: "git executable unavailable",
    };
    SourceControlFixture(failed);
    const panel = SourceControlFixture(failed);
    expect(panel).toMatchObject({
      statusError: failed.localError,
      panelState: "error",
    });
    await panel.refresh();
    expect(summary.refresh).toHaveBeenCalledWith({ remote: "never" });
    SourceControlFixture(summary);
    expect(SourceControlFixture(summary).panelState).toBe("ready");
  });

  it("retains the successful no-repository result and clears an old-context error", () => {
    const failed = {
      ...summary,
      repo: null,
      status: null,
      hasRepo: false,
      localError: "old context",
    };
    SourceControlFixture(failed);
    expect(SourceControlFixture(failed).panelState).toBe("error");
    const outside = { ...failed, localError: null };
    SourceControlFixture(outside);
    expect(SourceControlFixture(outside)).toMatchObject({
      statusError: null,
      panelState: "no-repo",
    });
  });
});
