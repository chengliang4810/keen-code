import { describe, expect, it } from "vitest";
import {
  buildMobileRemoteUrl,
  displayWebHostHost,
  isLoopbackBind,
} from "./webHostUrl";

describe("displayWebHostHost", () => {
  it("IPv4 与已带方括号的地址原样返回", () => {
    expect(displayWebHostHost("192.168.1.5")).toBe("192.168.1.5");
    expect(displayWebHostHost("[::1]")).toBe("[::1]");
  });

  it("IPv6 字面量补方括号", () => {
    expect(displayWebHostHost("::1")).toBe("[::1]");
    expect(displayWebHostHost("fe80::1")).toBe("[fe80::1]");
  });
});

describe("isLoopbackBind", () => {
  it("识别回环地址与主机名", () => {
    expect(isLoopbackBind("127.0.0.1")).toBe(true);
    expect(isLoopbackBind("::1")).toBe(true);
    expect(isLoopbackBind("localhost")).toBe(true);
    expect(isLoopbackBind(" 127.0.0.1 ")).toBe(true);
  });

  it("局域网地址不算回环", () => {
    expect(isLoopbackBind("192.168.1.20")).toBe(false);
    expect(isLoopbackBind("10.0.0.2")).toBe(false);
  });
});

describe("buildMobileRemoteUrl", () => {
  it("生成带宿主模式与 token 的配对链接", () => {
    expect(buildMobileRemoteUrl("192.168.1.20", 8080, "secret-token")).toBe(
      "http://192.168.1.20:8080/?host=mobile-remote&token=secret-token",
    );
  });

  it("token 中的保留字符经过 URL 编码，IPv6 补方括号", () => {
    expect(buildMobileRemoteUrl("::1", 80, "a+b/c")).toBe(
      "http://[::1]:80/?host=mobile-remote&token=a%2Bb%2Fc",
    );
  });

  it("缺 token 或端口非法时拒绝生成", () => {
    expect(buildMobileRemoteUrl("192.168.1.20", 8080, "  ")).toBeNull();
    expect(buildMobileRemoteUrl("192.168.1.20", 0, "t")).toBeNull();
    expect(buildMobileRemoteUrl("192.168.1.20", 70_000, "t")).toBeNull();
  });
});
