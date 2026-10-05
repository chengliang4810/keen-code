import { describe, expect, it } from "vitest";
import type {
  IZCodeAgentService,
  ZCodeAgentAttachmentPreviewSourceParams,
} from "./zcodeAgent.js";
import {
  createZCodeAgentConnectionScope,
  readTrustedZCodeAgentV4Connection,
} from "./zcodeAgentConnectionScope.js";

describe("ZCodeAgentConnectionScope attachment preview", () => {
  it("injects the connection mode and overwrites a caller supplied mode", async () => {
    const forwarded: unknown[] = [];
    const base = {
      attachmentPreviewSourceV4: async (params: unknown) => {
        forwarded.push(params);
        return { kind: "chunked" as const };
      },
    } as unknown as IZCodeAgentService;
    const scope = createZCodeAgentConnectionScope(base, {
      connectionId: "desktop-connection",
      clientMode: "desktop-continuous",
      role: "trusted-host-relay",
    });

    await scope.service.attachmentPreviewSourceV4({
      workspacePath: "C:/workspace",
      sessionId: "session-1",
      ref: "keencode-attachment://asset-1",
    });
    await scope.service.attachmentPreviewSourceV4({
      workspacePath: "C:/workspace",
      sessionId: "session-1",
      ref: "keencode-attachment://asset-2",
      clientMode: "web-remote-replayable",
    } as unknown as ZCodeAgentAttachmentPreviewSourceParams);

    expect(forwarded).toHaveLength(2);
    for (const value of forwarded) {
      expect((value as Record<string, unknown>).clientMode).toBe("desktop-continuous");
      expect(readTrustedZCodeAgentV4Connection(value)?.clientMode).toBe("desktop-continuous");
    }
    await scope.dispose();
  });
});
