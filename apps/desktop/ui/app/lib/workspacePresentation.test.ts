import { workspacePresentationId } from "@/app/lib/workspacePresentation";
import { resolveAgentNotificationDelivery } from "@/modules/agents/lib/delivery";
import { describe, expect, it } from "vitest";

describe("workspace presentation and terminal notifications", () => {
  it.each([
    [false, "workspace", true],
    [true, "git", true],
    [true, "explorer", true],
    [true, "workspace", false],
  ])("delivers attention for a hidden tool surface", (open, view, ready) => {
    const activeId = workspacePresentationId(7, open, view, ready);
    expect(activeId).toBe(-1);
    expect(resolveAgentNotificationDelivery({
      focused: true,
      visible: activeId === 7,
      allowToast: true,
    })).toBe("toast");
  });

  it("suppresses attention only for the visible active terminal", () => {
    const activeId = workspacePresentationId(7, true, "workspace", true);
    expect(activeId).toBe(7);
    expect(resolveAgentNotificationDelivery({
      focused: true,
      visible: activeId === 7,
      allowToast: true,
    })).toBe("none");
    expect(resolveAgentNotificationDelivery({
      focused: true,
      visible: activeId === 8,
      allowToast: true,
    })).toBe("toast");
  });
});
