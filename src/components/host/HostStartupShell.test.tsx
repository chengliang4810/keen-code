import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { HostAuthenticatedContent, HostStartupShell } from "./HostStartupShell";

const hostStartupSource = readFileSync(new URL("./HostStartupShell.tsx", import.meta.url), "utf8");

describe("HostStartupShell", () => {
  it("Desktop 直接保留原 App 树，不增加宿主门面节点", () => {
    const html = renderToStaticMarkup(
      <HostStartupShell hostMode="desktop">
        <div data-testid="app-tree">桌面工作台</div>
      </HostStartupShell>,
    );
    expect(html).toBe('<div data-testid="app-tree">桌面工作台</div>');
  });

  it("Web 未注入 transport 时停在登录入口，不伪造已登录", () => {
    const html = renderToStaticMarkup(
      <HostStartupShell hostMode="web" transport={null}>
        <div data-testid="app-tree">远程工作台</div>
      </HostStartupShell>,
    );
    expect(html).toContain('data-host-mode="web"');
    expect(html).toContain('data-status="signed-out"');
    expect(html).not.toContain('data-testid="app-tree"');
  });

  it("Web 认证后挂载完整工作台，不降级为移动远程壳", () => {
    const html = renderToStaticMarkup(
      <HostAuthenticatedContent hostMode="web" transport={null}>
        <div data-testid="app-tree">完整 Web 工作台</div>
      </HostAuthenticatedContent>,
    );
    expect(html).toContain('data-testid="web-workspace-shell"');
    expect(html).toContain('data-testid="app-tree"');
    expect(html).not.toContain('data-testid="mobile-remote-shell"');
  });

  it("Mobile Remote 未注入 transport 时展示未授权状态，不挂载桌面 App", () => {
    const html = renderToStaticMarkup(
      <HostStartupShell hostMode="mobile-remote" transport={null}>
        <div data-testid="app-tree">桌面工作台</div>
      </HostStartupShell>,
    );
    expect(html).toContain('data-host-mode="mobile-remote"');
    expect(html).toContain('data-connection="unauthorized"');
    expect(html).not.toContain('data-testid="app-tree"');
    expect(html).not.toContain("Terminal");
  });

  it("独立远程根壳同步 visual viewport，软键盘不会复用桌面 App 的高度假设", () => {
    expect(hostStartupSource).toContain("useVisualViewportLayout");
    expect(hostStartupSource).toContain("Remote 根壳独立同步 visual viewport");
  });
});
