import { afterEach, describe, expect, it, vi } from "vitest";
import {
  focusFirst,
  listFocusable,
  trapTabKey,
} from "./a11yFocus";

type FakeEl = HTMLElement & {
  id: string;
  disabled?: boolean;
  _focus: ReturnType<typeof vi.fn>;
};

function fakeEl(
  id: string,
  opts?: { disabled?: boolean },
): FakeEl {
  const focus = vi.fn();
  const el = {
    id,
    hasAttribute: (n: string) => n === "disabled" && !!opts?.disabled,
    getAttribute: (_n: string) => null as string | null,
    focus,
    _focus: focus,
  };
  return el as unknown as FakeEl;
}

function fakeRoot(els: FakeEl[]): ParentNode {
  return {
    querySelectorAll: () => els,
    contains: (n: Node) => els.includes(n as FakeEl),
  } as unknown as ParentNode;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("listFocusable / focusFirst", () => {
  it("lists enabled controls and skips disabled", () => {
    vi.stubGlobal("window", {
      getComputedStyle: () => ({ visibility: "visible", display: "block" }),
    });
    const a = fakeEl("a");
    const b = fakeEl("b");
    const disabled = fakeEl("x", { disabled: true });
    // disabled has hasAttribute true — listFocusable filters it
    const root = fakeRoot([disabled, a, b]);
    // querySelectorAll returns all; listFocusable filters disabled
    const list = listFocusable(root);
    expect(list.map((e) => (e as FakeEl).id)).toEqual(["a", "b"]);
    const focused = focusFirst(root);
    expect((focused as FakeEl | null)?.id).toBe("a");
    expect(a._focus).toHaveBeenCalled();
  });
});

describe("trapTabKey", () => {
  it("wraps from last to first on Tab", () => {
    vi.stubGlobal("window", {
      getComputedStyle: () => ({ visibility: "visible", display: "block" }),
    });
    const a = fakeEl("a");
    const b = fakeEl("b");
    const root = fakeRoot([a, b]);
    vi.stubGlobal("document", { activeElement: b });
    const e = {
      key: "Tab",
      shiftKey: false,
      preventDefault: vi.fn(),
    };
    trapTabKey(e, root);
    expect(e.preventDefault).toHaveBeenCalled();
    expect(a._focus).toHaveBeenCalled();
  });

  it("wraps from first to last on Shift+Tab", () => {
    vi.stubGlobal("window", {
      getComputedStyle: () => ({ visibility: "visible", display: "block" }),
    });
    const a = fakeEl("a");
    const b = fakeEl("b");
    const root = fakeRoot([a, b]);
    vi.stubGlobal("document", { activeElement: a });
    const e = {
      key: "Tab",
      shiftKey: true,
      preventDefault: vi.fn(),
    };
    trapTabKey(e, root);
    expect(e.preventDefault).toHaveBeenCalled();
    expect(b._focus).toHaveBeenCalled();
  });
});
