import { describe, expect, it } from "vitest";
import { highlightChatCode } from "./chatCodeHighlight";

describe("chatCodeHighlight", () => {
  it("uses the shared language aliases for settled chat fences", () => {
    expect(highlightChatCode("const value = 1", "tsx")).not.toBeNull();
    expect(highlightChatCode("<div />", "html")).not.toBeNull();
  });

  it("leaves plain and unknown fences unhighlighted", () => {
    expect(highlightChatCode("plain", "text")).toBeNull();
    expect(highlightChatCode("plain", "unknown-language")).toBeNull();
  });
});
