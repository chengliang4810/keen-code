import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  isUserMessageOverflowing,
  resetUserMessageExpanded,
  toggleUserMessageExpanded,
  USER_MESSAGE_COLLAPSED_HEIGHT,
  UserMessageBody,
} from "./UserMessageBody";

describe("UserMessageBody", () => {
  it("只在实际内容超过折叠高度时判定为溢出", () => {
    expect(isUserMessageOverflowing(USER_MESSAGE_COLLAPSED_HEIGHT)).toBe(false);
    expect(isUserMessageOverflowing(USER_MESSAGE_COLLAPSED_HEIGHT + 1)).toBe(false);
    expect(isUserMessageOverflowing(USER_MESSAGE_COLLAPSED_HEIGHT + 2)).toBe(true);
  });

  it("展开和收起在两个稳定状态之间切换", () => {
    expect(toggleUserMessageExpanded(false)).toBe(true);
    expect(toggleUserMessageExpanded(true)).toBe(false);
  });

  it("contentKey 改变时复位展开状态，同一内容保留当前状态", () => {
    expect(resetUserMessageExpanded(true, "message-1", "message-2")).toBe(false);
    expect(resetUserMessageExpanded(false, "message-1", "message-2")).toBe(false);
    expect(resetUserMessageExpanded(true, "message-1", "message-1")).toBe(true);
  });

  it("服务端首屏保持折叠结构并保留消息正文", () => {
    const html = renderToString(
      <UserMessageBody
        contentKey="message-1"
        expandLabel="Expand message"
        collapseLabel="Collapse message"
      >
        <span>Long user message</span>
      </UserMessageBody>,
    );

    expect(html).toContain('class="lobe-chat-user-body"');
    expect(html).toContain('lobe-chat-user-body__content');
    expect(html).toContain("Long user message");
    expect(html).toContain(`--user-message-content-height:${USER_MESSAGE_COLLAPSED_HEIGHT}px`);
  });
});
