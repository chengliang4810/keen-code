import { describe, expect, it } from "vitest";
import {
  buildComposerMentionMarkdown,
  composerMentionTriggerForKind,
  decodeComposerMentionToken,
  detectComposerMentionQuery,
  encodeComposerMention,
  filterComposerMentions,
  isComposerMention,
  insertComposerMentionAtCaret,
  removeComposerMentionAtCaret,
  type ComposerMention,
} from "./composerMentions";

function mention(kind: ComposerMention["kind"], label: string, value: string): ComposerMention {
  return {
    id: `${kind}:${value}`,
    kind,
    label,
    value,
    markdown: buildComposerMentionMarkdown(kind, label, value),
    description: `description for ${label}`,
    data: {
      path: kind === "file" || kind === "directory" ? value : undefined,
      sessionId: kind === "session" ? value : undefined,
      pluginId: kind === "plugin" ? value : undefined,
      skillName: kind === "skill" ? value : undefined,
    },
  };
}

/** contentEditable 行为的最小 DOM 桩；测试环境保持 node，不引入 jsdom。 */
class FakeNode {
  nodeType: number;
  textContent: string;
  parentNode: FakeNode | null = null;
  childNodes: FakeNode[] = [];
  dataset: Record<string, string> = {};

  constructor(nodeType: number, textContent = "") {
    this.nodeType = nodeType;
    this.textContent = textContent;
  }

  append(...nodes: FakeNode[]): void {
    for (const node of nodes) {
      node.parentNode = this;
      this.childNodes.push(node);
    }
  }

  contains(target: FakeNode): boolean {
    if (target === this) return true;
    return this.childNodes.some((child) => child.contains(target));
  }

  get lastChild(): FakeNode | null {
    return this.childNodes.at(-1) ?? null;
  }

  get length(): number {
    return this.textContent.length;
  }

  removeChild(node: FakeNode): void {
    const index = this.childNodes.indexOf(node);
    if (index >= 0) this.childNodes.splice(index, 1);
    node.parentNode = null;
  }

  focus(): void {}

  setAttribute(_name: string, _value: string): void {}
}

class FakeSelection {
  anchorNode: FakeNode | null = null;
  anchorOffset = 0;
  isCollapsed = true;
  addRange(range: FakeRange): void {
    this.anchorNode = range.startContainer;
    this.anchorOffset = range.startOffset;
    this.isCollapsed = range.startContainer === range.endContainer &&
      range.startOffset === range.endOffset;
  }
  removeAllRanges(): void {}
}

class FakeRange {
  startContainer!: FakeNode;
  startOffset = 0;
  endContainer!: FakeNode;
  endOffset = 0;

  setStart(node: FakeNode, offset: number): void {
    this.startContainer = node;
    this.startOffset = offset;
  }
  setEnd(node: FakeNode, offset: number): void {
    this.endContainer = node;
    this.endOffset = offset;
  }
  selectNode(node: FakeNode): void {
    const parent = node.parentNode!;
    this.startContainer = parent;
    this.startOffset = parent.childNodes.indexOf(node);
    this.endContainer = parent;
    this.endOffset = this.startOffset + 1;
  }
  deleteContents(): void {
    if (this.startContainer === this.endContainer && this.startContainer.nodeType === 3) {
      const text = this.startContainer.textContent;
      this.startContainer.textContent = text.slice(0, this.startOffset) + text.slice(this.endOffset);
      return;
    }
    if (this.startContainer === this.endContainer) {
      const removed = this.startContainer.childNodes.splice(this.startOffset, this.endOffset - this.startOffset);
      for (const node of removed) node.parentNode = null;
    }
  }
  collapse(toStart: boolean): void {
    if (toStart) {
      this.endContainer = this.startContainer;
      this.endOffset = this.startOffset;
    }
  }
  insertNode(node: FakeNode): void {
    const parent = this.startContainer.parentNode!;
    const text = this.startContainer.textContent;
    const before = new FakeNode(3, text.slice(0, this.startOffset));
    const after = new FakeNode(3, text.slice(this.startOffset));
    const index = parent.childNodes.indexOf(this.startContainer);
    parent.childNodes.splice(index, 1);
    const inserted = node.childNodes.length ? node.childNodes : [node];
    for (const child of inserted) child.parentNode = parent;
    if (before.textContent) {
      before.parentNode = parent;
      parent.childNodes.splice(index, 0, before);
    }
    parent.childNodes.splice(index + (before.textContent ? 1 : 0), 0, ...inserted);
    if (after.textContent) {
      after.parentNode = parent;
      parent.childNodes.splice(index + (before.textContent ? 1 : 0) + inserted.length, 0, after);
    }
  }
}

function withFakeDom<T>(run: (selection: FakeSelection) => T): T {
  const selection = new FakeSelection();
  const previousDocument = (globalThis as { document?: unknown }).document;
  const previousWindow = (globalThis as { window?: unknown }).window;
  (globalThis as { document?: unknown }).document = {
    createRange: () => new FakeRange(),
    createElement: () => new FakeNode(1),
    createTextNode: (value: string) => new FakeNode(3, value),
    createDocumentFragment: () => new FakeNode(11),
  };
  (globalThis as { window?: unknown }).window = {
    getSelection: () => selection,
  };
  try {
    return run(selection);
  } finally {
    if (previousDocument === undefined) delete (globalThis as { document?: unknown }).document;
    else (globalThis as { document?: unknown }).document = previousDocument;
    if (previousWindow === undefined) delete (globalThis as { window?: unknown }).window;
    else (globalThis as { window?: unknown }).window = previousWindow;
  }
}

function fakeMention(value = "[[mention:token]]"): FakeNode {
  const node = new FakeNode(1);
  node.dataset.composerMention = value;
  node.append(new FakeNode(3, "@file"));
  return node;
}

describe("composer mentions", () => {
  it("round-trips all supported mention kinds without losing identity", () => {
    const entries = [
      mention("file", "report.md", "/tmp/report.md"),
      mention("directory", "src", "/tmp/project/src"),
      mention("session", "Build", "session-1"),
      mention("plugin", "reviewer", "reviewer@marketplace"),
      mention("skill", "code-review", "code-review"),
    ];

    for (const entry of entries) {
      const decoded = decodeComposerMentionToken(encodeComposerMention(entry));
      expect(decoded).toEqual(entry);
      expect(isComposerMention(decoded)).toBe(true);
    }
  });

  it("uses canonical markdown for files, folders, sessions, plugins, and skills", () => {
    expect(buildComposerMentionMarkdown("file", "report.md", "/tmp/report.md")).toBe(
      "[report.md](/tmp/report.md)",
    );
    expect(buildComposerMentionMarkdown("directory", "src", "/tmp/src/")).toBe(
      "[src](/tmp/src/)",
    );
    expect(buildComposerMentionMarkdown("session", "Build", "session-1")).toBe(
      "[#Build](#session-1)",
    );
    expect(buildComposerMentionMarkdown("session", "session-1", "session-1")).toBe(
      "#session-1",
    );
    expect(buildComposerMentionMarkdown("plugin", "reviewer", "reviewer@marketplace")).toBe(
      "[@reviewer](plugin://reviewer@marketplace)",
    );
    expect(buildComposerMentionMarkdown("skill", "code-review", "code-review")).toBe(
      "$code-review",
    );
  });

  it("only detects a final @ query at a token boundary", () => {
    expect(detectComposerMentionQuery("@repo")).toEqual({
      trigger: "@",
      start: 0,
      query: "repo",
      end: 5,
    });
    expect(detectComposerMentionQuery("text @src ")).toEqual({
      trigger: "@",
      start: 5,
      query: "src",
      end: 9,
    });
    expect(detectComposerMentionQuery("email@example.com")).toBeNull();
    expect(detectComposerMentionQuery("@one @two")).toEqual({
      trigger: "@",
      start: 5,
      query: "two",
      end: 9,
    });
  });

  it("detects @ files/plugins, # sessions, and $ skills independently", () => {
    expect(detectComposerMentionQuery("@src")).toMatchObject({
      trigger: "@",
      query: "src",
    });
    expect(detectComposerMentionQuery("open #Build")).toMatchObject({
      trigger: "#",
      start: 5,
      query: "Build",
    });
    expect(detectComposerMentionQuery("run $code-review")).toMatchObject({
      trigger: "$",
      start: 4,
      query: "code-review",
    });
    expect(detectComposerMentionQuery("cost$code-review")).toBeNull();
    expect(detectComposerMentionQuery("#one/$two")).toBeNull();
  });

  it("filters only candidates belonging to the active trigger", () => {
    const entries = [
      mention("file", "README.md", "/tmp/README.md"),
      mention("plugin", "reviewer", "reviewer@marketplace"),
      mention("session", "Build", "session-1"),
      mention("skill", "code-review", "code-review"),
    ];
    expect(
      filterComposerMentions(entries, {
        trigger: "@",
        start: 0,
        query: "",
        end: 1,
      }).map((entry) => entry.kind),
    ).toEqual(["file", "plugin"]);
    expect(
      filterComposerMentions(entries, {
        trigger: "#",
        start: 0,
        query: "build",
        end: 6,
      }).map((entry) => entry.kind),
    ).toEqual(["session"]);
    expect(
      filterComposerMentions(entries, {
        trigger: "$",
        start: 0,
        query: "review",
        end: 7,
      }).map((entry) => entry.kind),
    ).toEqual(["skill"]);
    expect(composerMentionTriggerForKind("session")).toBe("#");
    expect(composerMentionTriggerForKind("skill")).toBe("$");
  });

  it("rejects malformed tokens instead of treating ordinary text as a mention", () => {
    expect(decodeComposerMentionToken("[[mention:not-json]]")).toBeNull();
    expect(decodeComposerMentionToken("@report.md")).toBeNull();
  });

  it("在嵌套 DOM 的元素边界也能插入 mention（模拟 IME 完成后的 selection）", () => {
    withFakeDom((selection) => {
      const root = new FakeNode(1);
      const block = new FakeNode(1);
      const text = new FakeNode(3, "请看 @rep");
      block.append(text);
      root.append(block);
      selection.anchorNode = block;
      selection.anchorOffset = 1;

      expect(insertComposerMentionAtCaret(root as unknown as HTMLElement, mention("file", "report.md", "/tmp/report.md"))).toBe(true);
      expect(block.childNodes.some((node) => Boolean(node.dataset.composerMention))).toBe(true);
      expect(selection.anchorNode?.textContent).toBe(" ");
      expect(selection.anchorOffset).toBe(1);
    });
  });

  it("Backspace/Delete 在 mention 左右边界删除完整原子节点并保留光标", () => {
    withFakeDom((selection) => {
      const root = new FakeNode(1);
      const left = new FakeNode(3, "a");
      const chip = fakeMention();
      const right = new FakeNode(3, "b");
      root.append(left, chip, right);

      // Backspace at the right text boundary should remove the mention before it.
      selection.anchorNode = right;
      selection.anchorOffset = 0;
      expect(removeComposerMentionAtCaret(root as unknown as HTMLElement, "backward")).toBe(true);
      expect(root.childNodes).toEqual([left, right]);

      const secondChip = fakeMention("[[mention:token-2]]");
      root.childNodes.splice(1, 0, secondChip);
      secondChip.parentNode = root;
      selection.anchorNode = root;
      selection.anchorOffset = 1;
      expect(removeComposerMentionAtCaret(root as unknown as HTMLElement, "forward")).toBe(true);
      expect(root.childNodes).toEqual([left, right]);
      expect(selection.anchorNode).toBe(root);
      expect(selection.anchorOffset).toBe(1);
    });
  });
});
