import { describe, expect, it } from "vitest";
import { browserTabLabel, parseBrowserAddress } from "./browserTabs";

describe("parseBrowserAddress", () => {
  it("保留显式的 http 与 https 地址", () => {
    expect(parseBrowserAddress("https://example.com/a?b=1")).toEqual({
      kind: "web",
      url: "https://example.com/a?b=1",
    });
    expect(parseBrowserAddress("http://example.com")).toEqual({
      kind: "web",
      url: "http://example.com",
    });
  });

  it("为主机名补全协议，回环地址使用 http", () => {
    expect(parseBrowserAddress("example.com/docs")).toEqual({
      kind: "web",
      url: "https://example.com/docs",
    });
    expect(parseBrowserAddress("localhost:3000")).toEqual({
      kind: "web",
      url: "http://localhost:3000",
    });
    expect(parseBrowserAddress("127.0.0.1:8080/app")).toEqual({
      kind: "web",
      url: "http://127.0.0.1:8080/app",
    });
  });

  it("把本地绝对路径交给文件预览", () => {
    expect(parseBrowserAddress("/Users/me/report.html")).toEqual({
      kind: "file",
      path: "/Users/me/report.html",
    });
    expect(parseBrowserAddress("C:\\docs\\report.html")).toEqual({
      kind: "file",
      path: "C:\\docs\\report.html",
    });
  });

  it("从 file 地址还原本地路径", () => {
    expect(parseBrowserAddress("file:///Users/me/a%20b.html")).toEqual({
      kind: "file",
      path: "/Users/me/a b.html",
    });
    expect(parseBrowserAddress("file:///C:/docs/report.html")).toEqual({
      kind: "file",
      path: "C:/docs/report.html",
    });
  });

  it("拒绝脚本协议与空输入", () => {
    expect(parseBrowserAddress("javascript:alert(1)")).toBeNull();
    expect(parseBrowserAddress("data:text/html,<h1>x</h1>")).toBeNull();
    expect(parseBrowserAddress("   ")).toBeNull();
  });
});

describe("browserTabLabel", () => {
  it("使用主机名作为网页标签名", () => {
    expect(browserTabLabel("https://github.com/a/b")).toBe("github.com");
  });

  it("本地文件使用文件名", () => {
    expect(browserTabLabel("file:///Users/me/report.html")).toBe("report.html");
  });

  it("无法解析时回退到原串", () => {
    expect(browserTabLabel("not a url")).toBe("not a url");
  });
});
