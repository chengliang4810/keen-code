import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { isValidWebHostToken } from "./WebHostSettingsPanel";

describe("WebHostSettingsPanel Token validation", () => {
  it("matches the Rust UTF-8 byte limits without trimming the secret", () => {
    expect(isValidWebHostToken("token-123")).toBe(true);
    expect(isValidWebHostToken(" token-123")).toBe(false);
    expect(isValidWebHostToken("token-123\n")).toBe(false);
    expect(isValidWebHostToken("口".repeat(170))).toBe(true);
    expect(isValidWebHostToken("口".repeat(171))).toBe(false);
  });

  it("rejects short values and Unicode control characters", () => {
    expect(isValidWebHostToken("1234567")).toBe(false);
    expect(isValidWebHostToken(`token-123${String.fromCharCode(0x90)}`)).toBe(false);
  });
});

describe("Web Host release wiring", () => {
  it("bundles the built frontend under the runtime web resource root", () => {
    const config = JSON.parse(readFileSync(
      new URL("../../../desktop/tauri.conf.json", import.meta.url),
      "utf8",
    ));
    const backend = readFileSync(
      new URL("../../../desktop/src/web_host.rs", import.meta.url),
      "utf8",
    );
    expect(config.bundle.resources["../ui/dist/"]).toBe("web/");
    expect(backend).toContain('.join("web")');
  });
});
