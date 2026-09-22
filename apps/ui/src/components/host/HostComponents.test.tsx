import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import {
  MobileRemoteShell,
  type MobileRemoteConnection,
} from "./MobileRemoteShell";
import { WebLoginPanel, type WebLoginStatus } from "./WebLoginPanel";

const noop = vi.fn();
const mobileRemoteSource = readFileSync(new URL("./MobileRemoteShell.tsx", import.meta.url), "utf8");
const hostModeCss = readFileSync(new URL("./host-mode.css", import.meta.url), "utf8");

describe("host mode components", () => {
  it("Web 登录状态由 props 驱动，并在非 Web 宿主显示受限提示", () => {
    const statuses: readonly WebLoginStatus[] = ["signed-out", "submitting", "error", "signed-in"];
    for (const status of statuses) {
      const html = renderToStaticMarkup(
        <WebLoginPanel
          hostMode="web"
          status={status}
          token="test-token"
          onTokenChange={noop}
          onSubmit={noop}
          errorMessage="示例错误"
        />,
      );
      expect(html).toContain('data-host-mode="web"');
      expect(html).toContain(`data-status="${status}"`);
      expect(html).toContain('data-slot="input"');
      if (status === "error") expect(html).toContain("示例错误");
      if (status === "signed-in") expect(html).toContain("已登录");
    }

    const desktopHtml = renderToStaticMarkup(
      <WebLoginPanel
        hostMode="desktop"
        status="signed-out"
        token=""
        onTokenChange={noop}
        onSubmit={noop}
      />,
    );
    expect(desktopHtml).toContain("当前宿主不需要 Web 登录");
    expect(desktopHtml).not.toContain('data-slot="input"');
  });

  it("移动远程壳展示连接、询问和会话状态，但不暴露桌面路径或终端入口", () => {
    const states: readonly MobileRemoteConnection[] = [
      "connecting",
      "connected",
      "reconnecting",
      "offline",
      "unauthorized",
    ];
    for (const connection of states) {
      const html = renderToStaticMarkup(
        <MobileRemoteShell
          hostMode="mobile-remote"
          connection={connection}
          asking={connection === "connected"}
          session={{
            title: "远程会话",
            summary: "正在处理请求",
            status: "running",
          }}
          onReconnect={noop}
          onSend={noop}
          onUploadAttachment={async () => ({
            resourceId: "resource_1",
            fileName: "photo.png",
            contentType: "image/png",
            size: 4,
            previewUrl: "/api/resources/resource_1",
          })}
        >
          <p>远程内容</p>
        </MobileRemoteShell>,
      );
      expect(html).toContain('data-host-mode="mobile-remote"');
      expect(html).toContain(`data-connection="${connection}"`);
      expect(html).toContain("远程会话");
      expect(html).not.toContain("Terminal");
      expect(html).not.toContain("tauri");
      expect(html).toContain("添加附件");
    }
  });

  it("移动远程流式消息只在吸底状态跟随，并提供可操作的回到底部入口", () => {
    expect(mobileRemoteSource).toContain('useStickToBottom({');
    expect(mobileRemoteSource).toContain("if (isPinnedRef.current) scrollToBottom(\"instant\")");
    expect(mobileRemoteSource).toContain('onClick={() => scrollToBottom("smooth")}');
    expect(mobileRemoteSource).toContain('data-testid="mobile-remote-back-to-bottom"');
    expect(mobileRemoteSource).not.toContain("scrollIntoView");
  });

  it("移动会话抽屉通过遮罩与 Escape 关闭，并阻断底层点击", () => {
    expect(mobileRemoteSource).toContain('data-testid="mobile-remote-session-backdrop"');
    expect(mobileRemoteSource).toContain('event.key !== "Escape"');
    expect(mobileRemoteSource).toContain("setShowSessions(false)");
    expect(hostModeCss).toContain(".mobile-remote-shell__session-backdrop");
    expect(hostModeCss).toContain("z-index: 2");
  });

  it("独立移动壳使用 visual viewport 高度和三层 safe-area 保护", () => {
    expect(hostModeCss).toContain("var(--visual-viewport-height, 100dvh)");
    expect(hostModeCss).toContain("env(safe-area-inset-top, 0px)");
    expect(hostModeCss).toContain("env(safe-area-inset-bottom, 0px)");
    expect(hostModeCss).toContain(".mobile-remote-shell__conversation");
    expect(hostModeCss).toContain(".mobile-remote-shell__composer");
  });
});
