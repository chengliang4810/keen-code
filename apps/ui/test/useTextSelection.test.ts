import { createElement, useEffect, useLayoutEffect, useRef } from "react";
import { describe, expect, it } from "vitest";

import {
  areTextSelectionValuesEqual,
  useTextSelection,
} from "../../../packages/ui/src/hooks/useTextSelection.ts";

type Listener = (event: TestEvent) => void;

class TestEvent {
  readonly type: string;
  readonly bubbles: boolean;
  readonly cancelable: boolean;
  defaultPrevented = false;
  target: TestNode | null = null;
  currentTarget: TestNode | null = null;
  private propagationStopped = false;

  constructor(type: string, init: { bubbles?: boolean; cancelable?: boolean } = {}) {
    this.type = type;
    this.bubbles = init.bubbles ?? false;
    this.cancelable = init.cancelable ?? false;
  }

  preventDefault() {
    if (this.cancelable) this.defaultPrevented = true;
  }

  stopPropagation() {
    this.propagationStopped = true;
  }

  isPropagationStopped() {
    return this.propagationStopped;
  }
}

class TestNode {
  readonly nodeType: number;
  readonly nodeName: string;
  readonly ownerDocument: TestDocument | null;
  parentNode: TestNode | null = null;
  readonly childNodes: TestNode[] = [];
  private readonly listeners = new Map<string, Listener[]>();

  constructor(nodeType: number, nodeName: string, ownerDocument: TestDocument | null) {
    this.nodeType = nodeType;
    this.nodeName = nodeName;
    this.ownerDocument = ownerDocument;
  }

  get parentElement() {
    return this.parentNode?.nodeType === 1 ? this.parentNode : null;
  }

  get firstChild() {
    return this.childNodes[0] ?? null;
  }

  get lastChild() {
    return this.childNodes[this.childNodes.length - 1] ?? null;
  }

  appendChild(child: TestNode) {
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    this.childNodes.push(child);
    return child;
  }

  insertBefore(child: TestNode, before: TestNode | null) {
    if (child.parentNode) child.parentNode.removeChild(child);
    child.parentNode = this;
    const index = before === null ? -1 : this.childNodes.indexOf(before);
    this.childNodes.splice(index < 0 ? this.childNodes.length : index, 0, child);
    return child;
  }

  removeChild(child: TestNode) {
    const index = this.childNodes.indexOf(child);
    if (index >= 0) {
      this.childNodes.splice(index, 1);
      child.parentNode = null;
    }
    return child;
  }

  addEventListener(type: string, listener: Listener) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type: string, listener: Listener) {
    this.listeners.set(
      type,
      (this.listeners.get(type) ?? []).filter((candidate) => candidate !== listener),
    );
  }

  dispatchEvent(event: TestEvent) {
    event.target = this;
    for (const listener of [...(this.listeners.get(event.type) ?? [])]) {
      event.currentTarget = this;
      listener(event);
      if (event.isPropagationStopped()) break;
    }
    if (event.bubbles && !event.isPropagationStopped() && this.parentNode) {
      this.parentNode.dispatchEvent(event);
    }
    return true;
  }

  contains(node: TestNode | null): boolean {
    for (let current = node; current; current = current.parentNode) {
      if (current === this) return true;
    }
    return false;
  }
}

class TestElement extends TestNode {
  readonly tagName: string;
  readonly namespaceURI = "http://www.w3.org/1999/xhtml";
  readonly style = {
    setProperty: () => undefined,
    removeProperty: () => undefined,
  } as Record<string, unknown>;
  private readonly attributes = new Map<string, string>();
  private text = "";

  constructor(tagName: string, ownerDocument: TestDocument) {
    super(1, tagName.toUpperCase(), ownerDocument);
    this.tagName = tagName.toUpperCase();
  }

  setAttribute(name: string, value: unknown) {
    this.attributes.set(name, String(value));
  }

  removeAttribute(name: string) {
    this.attributes.delete(name);
  }

  hasAttribute(name: string) {
    return this.attributes.has(name);
  }

  getAttribute(name: string) {
    return this.attributes.get(name) ?? null;
  }

  set textContent(value: string) {
    this.text = value;
    for (const child of [...this.childNodes]) this.removeChild(child);
  }

  get textContent() {
    return this.text || this.childNodes.map((child) => child.textContent).join("");
  }

  get isConnected() {
    return this.ownerDocument?.contains(this) ?? false;
  }
}

class TestText extends TestNode {
  data: string;

  constructor(data: string, ownerDocument: TestDocument) {
    super(3, "#text", ownerDocument);
    this.data = data;
  }

  get textContent() {
    return this.data;
  }

  set textContent(value: string) {
    this.data = value;
  }
}

class TestDocument extends TestNode {
  readonly nodeType = 9;
  readonly nodeName = "#document";
  defaultView: TestWindow | null = null;
  readonly documentElement: TestElement;
  readonly body: TestElement;
  activeElement: TestElement;

  constructor() {
    super(9, "#document", null);
    this.documentElement = new TestElement("html", this);
    this.body = new TestElement("body", this);
    this.activeElement = this.body;
    this.appendChild(this.documentElement);
    this.documentElement.appendChild(this.body);
  }

  createElement(name: string) {
    return new TestElement(name, this);
  }

  createElementNS(_namespace: string, name: string) {
    return new TestElement(name, this);
  }

  createTextNode(value: string) {
    return new TestText(value, this);
  }

  createComment(value: string) {
    return new TestText(value, this);
  }
}

class TestWindow extends TestNode {
  readonly nodeType = 0;
  readonly nodeName = "#window";
  readonly navigator = { userAgent: "node" };
  readonly location = { protocol: "http:", host: "localhost" };
  readonly document: TestDocument;
  readonly frameCallbacks = new Map<number, (timestamp: number) => void>();
  selectionCollapsed = true;
  private nextFrameId = 1;

  constructor(document: TestDocument) {
    super(0, "#window", document);
    this.document = document;
  }

  requestAnimationFrame(callback: (timestamp: number) => void) {
    const id = this.nextFrameId++;
    this.frameCallbacks.set(id, callback);
    return id;
  }

  cancelAnimationFrame(id: number) {
    this.frameCallbacks.delete(id);
  }

  flushAnimationFrames() {
    const callbacks = [...this.frameCallbacks.values()];
    this.frameCallbacks.clear();
    callbacks.forEach((callback) => callback(0));
  }

  getSelection() {
    return { isCollapsed: this.selectionCollapsed };
  }
}

let inspectSelectionRevision = 0;
const inspectSelection = () => ({
  text: "selected",
  top: 12,
  bottom: 28,
  center: 40,
  reference: {
    id: `reference-${++inspectSelectionRevision}`,
    sourceSessionId: "session",
    sourceRowId: 1,
    contentType: "assistant",
    text: "selected",
  },
});

async function installTestDom() {
  const document = new TestDocument();
  const window = new TestWindow(document);
  document.defaultView = window;
  const globals = {
    window: globalThis.window,
    document: globalThis.document,
    Node: globalThis.Node,
    Element: globalThis.Element,
    HTMLElement: globalThis.HTMLElement,
    HTMLIFrameElement: globalThis.HTMLIFrameElement,
    Event: globalThis.Event,
  };
  Object.assign(globalThis, {
    window,
    document,
    Node: TestNode,
    Element: TestElement,
    HTMLElement: TestElement,
    HTMLIFrameElement: class extends TestElement {},
    Event: TestEvent,
  });
  Object.assign(window, {
    Node: TestNode,
    Element: TestElement,
    HTMLElement: TestElement,
    HTMLIFrameElement: globalThis.HTMLIFrameElement,
    Event: TestEvent,
  });
  const { act } = await import("react");
  const { createRoot } = await import("react-dom/client");
  return {
    document,
    window,
    act,
    createRoot,
    restore: () => Object.assign(globalThis, globals),
  };
}

function ScrollLoopProbe({
  onRender,
  observeSelectionChange = false,
  dispatchScrollOnLayout = true,
}: {
  onRender: (count: number) => void;
  observeSelectionChange?: boolean;
  dispatchScrollOnLayout?: boolean;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const armedRef = useRef(false);
  const renderCountRef = useRef(0);
  renderCountRef.current += 1;
  const { state } = useTextSelection({
    rootRef,
    enabled: true,
    inspect: inspectSelection,
    scopeKey: "session",
    observeSelectionChange,
  });
  useEffect(() => {
    armedRef.current = true;
  }, []);
  useLayoutEffect(() => {
    onRender(renderCountRef.current);
    if (!armedRef.current) return;
    if (!dispatchScrollOnLayout) return;
    if (observeSelectionChange) {
      rootRef.current?.ownerDocument?.dispatchEvent(new TestEvent("selectionchange"));
    } else {
      rootRef.current?.dispatchEvent(new TestEvent("scroll"));
    }
  });
  return createElement("div", { ref: rootRef, "data-selection": state ? "open" : "closed" });
}

describe("text selection snapshot stability", () => {
  it("reuses a snapshot when a scroll event reports identical geometry", () => {
    expect(
      areTextSelectionValuesEqual(
        { text: "selected", top: 12, bottom: 28, center: 40 },
        { text: "selected", top: 12, bottom: 28, center: 40 },
      ),
    ).toBe(true);
  });

  it("keeps changed selection geometry observable", () => {
    expect(
      areTextSelectionValuesEqual(
        { text: "selected", top: 12, bottom: 28, center: 40 },
        { text: "selected", top: 13, bottom: 29, center: 40 },
      ),
    ).toBe(false);
  });

  it("ignores regenerated selection reference ids while preserving source fields", () => {
    expect(
      areTextSelectionValuesEqual(
        {
          text: "selected",
          reference: {
            id: "old",
            contentType: "assistant",
            sourceSessionId: "session",
            sourceRowId: 1,
            text: "selected",
          },
        },
        {
          text: "selected",
          reference: {
            id: "new",
            contentType: "assistant",
            sourceSessionId: "session",
            sourceRowId: 1,
            text: "selected",
          },
        },
      ),
    ).toBe(true);
    expect(
      areTextSelectionValuesEqual(
        {
          text: "selected",
          reference: {
            id: "old",
            contentType: "assistant",
            sourceSessionId: "session",
            sourceRowId: 1,
            text: "selected",
          },
        },
        {
          text: "selected",
          reference: {
            id: "new",
            contentType: "assistant",
            sourceSessionId: "session",
            sourceRowId: 2,
            text: "selected",
          },
        },
      ),
    ).toBe(false);
    expect(
      areTextSelectionValuesEqual(
        { text: "selected", metadata: { id: "old" } },
        { text: "selected", metadata: { id: "new" } },
      ),
    ).toBe(false);
  });

  it("bounds a synthetic scroll to one close update and one rerender", async () => {
    const previousActEnvironment = globalThis.IS_REACT_ACT_ENVIRONMENT;
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    const dom = await installTestDom();
    const container = dom.document.createElement("div");
    dom.document.body.appendChild(container);
    const renderCounts: number[] = [];
    const root = dom.createRoot(container);

    try {
      await dom.act(async () => {
        root.render(createElement(ScrollLoopProbe, { onRender: (count) => renderCounts.push(count) }));
      });
      const scrollRoot = container.firstChild;
      expect(scrollRoot).toBeInstanceOf(TestElement);
      await dom.act(async () => {
        scrollRoot?.dispatchEvent(new TestEvent("mouseup", { bubbles: true }));
        dom.window.flushAnimationFrames();
        await Promise.resolve();
      });

      expect(renderCounts.length).toBeLessThan(8);
      expect((scrollRoot as TestElement).getAttribute("data-selection")).toBe("closed");
    } finally {
      await dom.act(async () => root.unmount());
      dom.restore();
      if (previousActEnvironment === undefined) {
        delete globalThis.IS_REACT_ACT_ENVIRONMENT;
      } else {
        globalThis.IS_REACT_ACT_ENVIRONMENT = previousActEnvironment;
      }
    }
  });

  it("defers a scroll close until the native event has returned", async () => {
    const previousActEnvironment = globalThis.IS_REACT_ACT_ENVIRONMENT;
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    const dom = await installTestDom();
    const container = dom.document.createElement("div");
    dom.document.body.appendChild(container);
    const renderCounts: number[] = [];
    const root = dom.createRoot(container);

    try {
      await dom.act(async () => {
        root.render(
          createElement(ScrollLoopProbe, {
            dispatchScrollOnLayout: false,
            onRender: (count) => renderCounts.push(count),
          }),
        );
      });
      const scrollRoot = container.firstChild;
      expect(scrollRoot).toBeInstanceOf(TestElement);
      await dom.act(async () => {
        scrollRoot?.dispatchEvent(new TestEvent("mouseup", { bubbles: true }));
        dom.window.flushAnimationFrames();
      });
      const openedRenderCount = renderCounts.length;

      scrollRoot?.dispatchEvent(new TestEvent("scroll"));
      // 合成 scroll 的监听器不能在 dispatchEvent 调用栈里同步推进 React。
      expect(renderCounts.length).toBe(openedRenderCount);

      await dom.act(async () => {
        await Promise.resolve();
      });
      expect(renderCounts.length).toBe(openedRenderCount + 1);
      expect((scrollRoot as TestElement).getAttribute("data-selection")).toBe("closed");
    } finally {
      await dom.act(async () => root.unmount());
      dom.restore();
      if (previousActEnvironment === undefined) {
        delete globalThis.IS_REACT_ACT_ENVIRONMENT;
      } else {
        globalThis.IS_REACT_ACT_ENVIRONMENT = previousActEnvironment;
      }
    }
  });

  it("deduplicates repeated selectionchange snapshots from the same DOM range", async () => {
    const previousActEnvironment = globalThis.IS_REACT_ACT_ENVIRONMENT;
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    const dom = await installTestDom();
    dom.window.selectionCollapsed = false;
    const container = dom.document.createElement("div");
    dom.document.body.appendChild(container);
    const renderCounts: number[] = [];
    const root = dom.createRoot(container);

    try {
      await dom.act(async () => {
        root.render(
          createElement(ScrollLoopProbe, {
            observeSelectionChange: true,
            onRender: (count) => renderCounts.push(count),
          }),
        );
      });
      const scrollRoot = container.firstChild;
      expect(scrollRoot).toBeInstanceOf(TestElement);
      await dom.act(async () => {
        scrollRoot?.dispatchEvent(new TestEvent("mouseup", { bubbles: true }));
      });
      for (let attempt = 0; attempt < 8; attempt += 1) {
        await dom.act(async () => {
          dom.window.flushAnimationFrames();
          await Promise.resolve();
        });
      }

      expect(renderCounts.length).toBeLessThan(8);
      expect(dom.window.frameCallbacks.size).toBe(0);
    } finally {
      await dom.act(async () => root.unmount());
      dom.restore();
      if (previousActEnvironment === undefined) {
        delete globalThis.IS_REACT_ACT_ENVIRONMENT;
      } else {
        globalThis.IS_REACT_ACT_ENVIRONMENT = previousActEnvironment;
      }
    }
  });
});
