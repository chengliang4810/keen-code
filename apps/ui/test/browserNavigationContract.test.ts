import { expect, test } from "vitest";

import {
  openBrowserSidePane,
  openOrActivateBrowserSidePaneByUrl,
} from "../../../packages/ui/src/lib/workspaceSidePane.ts";

const popupUrl = "http://127.0.0.1:8765/popup.html";
const owner = {
  ownerTaskId: "task-browser-contract",
  workspaceKey: "C:/isolated/browser-contract",
};

test("target blank opens one active browser tab with its initial URL", () => {
  const state = openBrowserSidePane(null, {
    tabId: "browser:popup",
    initialUrl: popupUrl,
    ...owner,
  });

  expect(state.tabs).toHaveLength(1);
  expect(state.activeTabId).toBe("browser:popup");
  expect(state.tabs[0]).toMatchObject({
    id: "browser:popup",
    type: "browser",
    initialUrl: popupUrl,
    ...owner,
  });
});

test("URL reuse keeps the existing tab and leaves navigation to the caller", () => {
  const initial = openBrowserSidePane(null, {
    tabId: "browser:existing",
    initialUrl: popupUrl,
    ...owner,
  });

  const reused = openOrActivateBrowserSidePaneByUrl(initial, {
    tabId: "browser:ignored-new-id",
    initialUrl: popupUrl,
    ...owner,
  });

  expect(reused.tabs).toHaveLength(1);
  expect(reused.activeTabId).toBe("browser:existing");
  expect(reused.tabs[0]?.id).toBe("browser:existing");
});
