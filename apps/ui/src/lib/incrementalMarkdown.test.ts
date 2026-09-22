import { describe, expect, it } from "vitest";
import {
  IncrementalMarkdownState,
  requiresFullMarkdownParse,
} from "./incrementalMarkdown";

const threeParagraphs = "one\n\ntwo\n\nthree";

describe("IncrementalMarkdownState", () => {
  it("falls back to a full document parse for global reference definitions", () => {
    expect(
      requiresFullMarkdownParse(
        "Earlier [reference][x].\n\n[x]: https://example.com",
      ),
    ).toBe(true);
    expect(
      requiresFullMarkdownParse(
        "Earlier [reference][x].\n\n[x]:\n  https://example.com",
      ),
    ).toBe(true);
    expect(requiresFullMarkdownParse("[inline](https://example.com)")).toBe(
      false,
    );
  });

  it("freezes parsed prefix blocks and leaves only the unstable tail", () => {
    const state = new IncrementalMarkdownState();
    const first = state.update(threeParagraphs);
    state.recordTailPositions(first.tail.key, first.tail.source, [
      { start: 0, end: 3 },
      { start: 5, end: 8 },
      { start: 10, end: 15 },
    ]);

    const next = state.update(`${threeParagraphs}!`);

    expect(next.frozen).toEqual([{ key: 0, source: "one" }]);
    expect(next.tail).toEqual({ key: 3, source: "\n\ntwo\n\nthree!" });
  });

  it("keeps frozen source stable while subsequent tail blocks advance", () => {
    const state = new IncrementalMarkdownState();
    const first = state.update(threeParagraphs);
    state.recordTailPositions(first.tail.key, first.tail.source, [
      { start: 0, end: 3 },
      { start: 5, end: 8 },
      { start: 10, end: 15 },
    ]);
    const second = state.update(`${threeParagraphs}\n\nfour`);
    state.recordTailPositions(second.tail.key, second.tail.source, [
      { start: 2, end: 5 },
      { start: 7, end: 12 },
      { start: 14, end: 18 },
    ]);

    const third = state.update(`${threeParagraphs}\n\nfour!`);

    expect(third.frozen).toEqual([
      { key: 0, source: "one" },
      { key: 3, source: "\n\ntwo" },
    ]);
    expect(third.tail.source).toBe("\n\nthree\n\nfour!");
  });

  it("drops frozen state when input is not append-only", () => {
    const state = new IncrementalMarkdownState();
    const first = state.update(threeParagraphs);
    state.recordTailPositions(first.tail.key, first.tail.source, [
      { start: 0, end: 3 },
      { start: 5, end: 8 },
      { start: 10, end: 15 },
    ]);
    expect(state.update(`${threeParagraphs}!`).frozen).toHaveLength(1);

    const reset = state.update("replacement");

    expect(reset.generation).toBe(1);
    expect(reset.frozen).toEqual([]);
    expect(reset.tail).toEqual({ key: 0, source: "replacement" });
  });

  it("ignores stale or invalid parse positions", () => {
    const state = new IncrementalMarkdownState();
    const frame = state.update(threeParagraphs);
    state.recordTailPositions(frame.tail.key + 1, frame.tail.source, [
      { start: 0, end: 3 },
      { start: 5, end: 8 },
      { start: 10, end: 15 },
    ]);
    expect(state.update(`${threeParagraphs}!`).frozen).toEqual([]);

    const current = state.update(`${threeParagraphs}!!`);
    state.recordTailPositions(current.tail.key, current.tail.source, [
      { start: 0, end: current.tail.source.length + 1 },
    ]);
    expect(state.update(`${threeParagraphs}!!!`).frozen).toEqual([]);
  });

  it("keeps cumulative parse input near the growing tail instead of the full reply", () => {
    const state = new IncrementalMarkdownState();
    let text = "";
    let incrementalChars = 0;
    let fullReparseChars = 0;

    for (let index = 0; index < 200; index += 1) {
      text += `${index === 0 ? "" : "\n\n"}paragraph-${index}`;
      const frame = state.update(text);
      incrementalChars += frame.tail.source.length;
      fullReparseChars += text.length;

      const positions = Array.from(
        frame.tail.source.matchAll(/paragraph-\d+/g),
        (match) => ({
          start: match.index,
          end: match.index + match[0].length,
        }),
      );
      state.recordTailPositions(frame.tail.key, frame.tail.source, positions);
    }

    expect(incrementalChars).toBeLessThan(fullReparseChars / 10);
    expect(state.update(text).tail.source.match(/paragraph-\d+/g)).toHaveLength(
      3,
    );
  });
});
