/**
 * 侧栏项目展开合并的回归测试：
 * 反复展开/收起项目不得把未登记项目的会话（"对话"分区的孤儿行）重复累积。
 */

import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Project } from "@/features/app/models";

const ports = vi.hoisted(() => ({
  /** useState/useRef 按渲染顺序对齐的 slot 序列。 */
  slots: [] as Array<{ value?: unknown; current?: unknown }>,
  cursor: 0,
  /** 最近一次渲染收集的 effect；测试只在初始化时执行。 */
  effects: [] as Array<() => unknown>,
  isTauri: true,
  projects: [] as Array<Project>,
  /** 按 cwd 过滤的会话列表，模拟 Host 的 `session/list`。 */
  list: [] as Array<{
    sessionId: string;
    cwd: string;
    title?: string;
    updatedAt?: string;
    _meta?: Record<string, unknown>;
  }>,
}));

vi.mock("react", async (original) => ({
  ...await original<typeof import("react")>(),
  useState: (init: unknown) => {
    const index = ports.cursor++;
    if (!ports.slots[index]) {
      ports.slots[index] = {
        value: typeof init === "function" ? (init as () => unknown)() : init,
      };
    }
    const slot = ports.slots[index]!;
    return [
      slot.value,
      (next: unknown) => {
        slot.value =
          typeof next === "function"
            ? (next as (previous: unknown) => unknown)(slot.value)
            : next;
      },
    ];
  },
  useRef: (initial: unknown) => {
    const index = ports.cursor++;
    if (!ports.slots[index]) ports.slots[index] = { current: initial };
    return ports.slots[index];
  },
  useMemo: (factory: () => unknown) => {
    ports.cursor += 1;
    return factory();
  },
  useCallback: (callback: unknown) => {
    ports.cursor += 1;
    return callback;
  },
  useEffect: (effect: () => unknown) => {
    ports.effects.push(effect);
  },
}));

vi.mock("@/lib/api", async (original) => ({
  ...await original<typeof import("@/lib/api")>(),
  isTauri: () => ports.isTauri,
  projectsList: async () => ports.projects,
  projectValidate: async (id: string) =>
    ports.projects.find((project) => project.id === id) ?? null,
}));

vi.mock("@/lib/acp/api", async (original) => ({
  ...await original<typeof import("@/lib/acp/api")>(),
  diagnosticsRecord: vi.fn(async () => undefined),
  sessionsList: async (cwd?: string) =>
    (cwd === undefined
      ? ports.list
      : ports.list.filter((item) => item.cwd === cwd)
    ).map((item) => ({
      id: item.sessionId,
      cwd: item.cwd,
      title: item.title ?? null,
      updatedAt: item.updatedAt ?? "",
      lastUserMessageAt:
        (item._meta?.["keencode/lastUserMessageAt"] as string | undefined) ?? null,
    })),
}));

const { useSidebarLists } = await import("./useSidebarLists");

/** node 测试环境无 localStorage，而默认参数在 try/catch 之外求值，需先桩好。 */
function stubLocalStorage() {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => {
      values.set(key, value);
    },
    removeItem: (key: string) => {
      values.delete(key);
    },
    clear: () => values.clear(),
  } satisfies Partial<Storage>);
}
stubLocalStorage();

const ALPHA: Project = {
  id: "alpha",
  name: "Alpha",
  path: "/projects/alpha",
  pathOk: true,
};
const ORPHAN_PATH = "/unregistered/beta";

function session(id: string, title: string, cwd: string) {
  return {
    sessionId: id,
    cwd,
    title,
    updatedAt: "2026-09-23T00:00:00Z",
    _meta: { "keencode/lastUserMessageAt": "2026-09-23T00:00:00Z" },
  };
}

/** 渲染一次 Hook 并返回最新结果；重复调用复用同一组 slot，模拟连续重渲染。 */
function renderLists() {
  ports.cursor = 0;
  let result!: ReturnType<typeof useSidebarLists>;
  function Harness() {
    result = useSidebarLists({
      locale: "zh",
      setActiveProject: vi.fn(),
      setAppBooting: vi.fn(),
      setLocalError: vi.fn(),
      showToast: vi.fn(),
    });
    return null;
  }
  renderToString(createElement(Harness));
  return result;
}

describe("useSidebarLists 项目展开合并", () => {
  beforeEach(() => {
    ports.slots.length = 0;
    ports.cursor = 0;
    ports.effects.length = 0;
    ports.isTauri = true;
    ports.projects = [ALPHA];
    ports.list = [
      session("chat-alpha-1", "计算1加1的程序", ALPHA.path),
      session("chat-orphan-1", "用户问候", ORPHAN_PATH),
    ];
  });

  it("反复展开/收起项目不重复累积孤儿会话，且项目会话幂等合并", async () => {
    let lists = renderLists();
    // 只执行初始化收集的 effect（refreshLists），后续重渲染不再重复。
    for (const effect of ports.effects.splice(0)) effect();
    await new Promise((resolve) => setTimeout(resolve, 0));
    lists = renderLists();
    expect(lists.projects.map((p) => p.id)).toContain(ALPHA.id);

    for (let round = 0; round < 3; round += 1) {
      await lists.toggleProject(lists.projects.find((p) => p.id === ALPHA.id)!);
      lists = renderLists();
      // 收起后再次展开，复现"每展开一次复制一份"的触发路径。
      await lists.toggleProject(lists.projects.find((p) => p.id === ALPHA.id)!);
      lists = renderLists();
    }

    expect(
      lists.sessions.filter((item) => item.id === "chat-orphan-1"),
    ).toHaveLength(1);
    expect(
      lists.sessions.filter((item) => item.id === "chat-alpha-1"),
    ).toHaveLength(1);
    expect(
      lists.sessions.find((item) => item.id === "chat-alpha-1")?.projectId,
    ).toBe(ALPHA.id);
  });
});
