import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { RemoteControlModalView } from "./RemoteControlModal";
import type { MessageKey } from "@/i18n";
import type { WebHostStatus } from "@/lib/api";

const tr = (key: MessageKey) => key;

const baseStatus: WebHostStatus = {
  state: "running",
  bind: "192.168.1.20",
  port: 8688,
  activeConnections: 0,
  maxConnections: 4,
  sessionCount: 1,
  tokenVersion: 1,
};

type ViewProps = Parameters<typeof RemoteControlModalView>[0];

const noop = () => {};

function renderView(overrides: Partial<ViewProps> = {}) {
  return renderToStaticMarkup(
    <RemoteControlModalView
      tr={tr}
      status={baseStatus}
      shareUrl={null}
      qrDataUrl={null}
      busy={false}
      copied={false}
      error={null}
      onToggleRun={noop}
      onRefresh={noop}
      onCopy={noop}
      onOpenSettings={noop}
      {...overrides}
    />,
  );
}

describe("RemoteControlModalView", () => {
  it("运行且监听局域网地址时展示扫码面板、停止按钮与二维码", () => {
    const html = renderView({
      shareUrl: "http://192.168.1.20:8688/?host=mobile-remote&token=secret",
      qrDataUrl: "data:image/png;base64,mock",
    });
    expect(html).toContain("remoteControl.section.title");
    expect(html).toContain("remoteControl.state.waiting");
    expect(html).toContain("remoteControl.state.ready");
    expect(html).toContain("remoteControl.action.stop");
    expect(html).toContain("remoteControl.action.refreshQr");
    expect(html).toContain("remoteControl.action.copyLink");
    expect(html).toContain('src="data:image/png;base64,mock"');
    expect(html).toContain('alt="remoteControl.qrAlt"');
    expect(html).not.toContain("remoteControl.loopbackWarning");
  });

  it("等待配对时显示引导文案，无链接时不渲染二维码区域", () => {
    const html = renderView();
    expect(html).toContain("remoteControl.hint.scan");
    expect(html).not.toContain("remoteControl.qrAlt");
  });

  it("监听回环地址时给出警告与设置入口，不渲染二维码", () => {
    const html = renderView({ status: { ...baseStatus, bind: "127.0.0.1" } });
    expect(html).toContain("remoteControl.loopbackWarning");
    expect(html).toContain("remoteControl.action.openSettings");
    expect(html).not.toContain("<img");
  });

  it("未运行时提供启动入口并复用设置页状态文案", () => {
    const html = renderView({ status: { ...baseStatus, state: "stopped" } });
    expect(html).toContain("remoteControl.action.start");
    expect(html).toContain("settings.webHost.state.stopped");
    expect(html).toContain("remoteControl.hint.offline");
  });

  it("加载失败只展示加载中与错误行，不伪造状态", () => {
    const html = renderView({ status: null, error: "remoteControl.loadError" });
    expect(html).toContain("remoteControl.state.loading");
    expect(html).toContain("remoteControl.loadError");
    expect(html).not.toContain("remoteControl.state.ready");
  });

  it("复制完成后按钮文案切换为已复制", () => {
    const html = renderView({
      shareUrl: "http://192.168.1.20:8688/?host=mobile-remote&token=secret",
      copied: true,
    });
    expect(html).toContain("remoteControl.copied");
    expect(html).not.toContain("remoteControl.action.copyLink");
  });
});

describe("RemoteControlModal 容器", () => {
  const source = readFileSync(new URL("./RemoteControlModal.tsx", import.meta.url), "utf8");

  it("打开时读取 Web Host 状态与 Token，并复用设置面板的状态缓存", () => {
    expect(source).toContain("api.webHostStatus()");
    expect(source).toContain("api.webHostGetToken()");
    expect(source).toContain('invalidateReadCache("web_host_status")');
  });

  it("启停动作走既有 Tauri 命令，二维码只消费过滤后的配对 URL", () => {
    expect(source).toContain("api.webHostStart()");
    expect(source).toContain("api.webHostStop()");
    expect(source).toContain("QRCode.toDataURL(shareUrl");
    expect(source).toContain("buildMobileRemoteUrl(status.bind, status.port, token)");
    expect(source).toContain("isLoopbackBind(status.bind)");
  });

  it("弹窗经 GlassModal 渲染，未引入原生控件", () => {
    expect(source).toContain("<GlassModal");
    expect(source).not.toMatch(/<(button|input|textarea|select)\b/);
  });
});
