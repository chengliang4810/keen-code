import { beforeEach, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { extensionApi } from "@/modules/ai/lib/extensions";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn().mockResolvedValue(undefined),
}));
beforeEach(() => vi.mocked(invoke).mockClear());
it("passes marketplace and update identities through the registered native commands", async () => {
  await extensionApi.marketplace();
  await extensionApi.initializeMarketplace();
  await extensionApi.addMarketplace("github:owner/repo");
  await extensionApi.refreshMarketplace("team");
  await extensionApi.refreshMarketplace(null, true);
  await extensionApi.installMarketplacePlugin("review@team");
  await extensionApi.details("review@team");
  await extensionApi.update("review@team");
  await extensionApi.update();
  await extensionApi.removeMarketplace("team");
  expect(vi.mocked(invoke).mock.calls).toEqual([
    ["agent_marketplace_catalog"],
    [
      "agent_marketplace_refresh",
      { name: null, restoreDefault: true, initializeOnly: true },
    ],
    ["agent_marketplace_add", { source: "github:owner/repo" }],
    ["agent_marketplace_refresh", { name: "team", restoreDefault: false }],
    ["agent_marketplace_refresh", { name: null, restoreDefault: true }],
    ["agent_marketplace_install", { pluginId: "review@team" }],
    ["agent_plugins_details", { pluginId: "review@team" }],
    ["agent_plugins_update", { pluginId: "review@team" }],
    ["agent_plugins_update", { pluginId: null }],
    ["agent_marketplace_remove", { name: "team" }],
  ]);
});
