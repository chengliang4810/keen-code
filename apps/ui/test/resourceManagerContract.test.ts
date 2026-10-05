import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  resourceUsageSnapshotSchema,
  storageUsageSnapshotSchema,
  STORAGE_CATEGORY_IDS,
} from "@zcode/shared";
import { expect, test } from "vitest";
import { formatPercent, groupResourceUsage } from "../../../packages/ui/src/resource-manager/resourceUsageView.ts";

const fixture = JSON.parse(
  readFileSync(
    resolve(
      import.meta.dirname,
      "../../../tooling/native-live/workflow-contract-fixtures/resource_manager.json",
    ),
    "utf8",
  ),
) as { resource: unknown; storage: unknown };

test("Rust ResourceManager DTO fixture satisfies strict resource and storage schemas", () => {
  const resource = resourceUsageSnapshotSchema.parse(fixture.resource);
  const storage = storageUsageSnapshotSchema.parse(fixture.storage);

  expect(resource.system.cpuPercent).toBeNull();
  expect(resource.app.cpuPercent).toBeNull();
  expect(resource.processes[0]?.cpuPercent).toBeNull();
  expect(resource.processes[0]?.sampled).toBe(false);
  expect(storage.status).toBe("complete");
  expect(storage.roots).toHaveLength(1);
  expect(storage.roots[0]?.categories.map((category) => category.id)).toEqual([
    ...STORAGE_CATEGORY_IDS,
  ]);
  expect(storage.roots[0]?.categories.every((category) => category.cleanability === "none")).toBe(
    true,
  );
});

test("unknown CPU samples remain visibly unknown in the resource view", () => {
  const resource = resourceUsageSnapshotSchema.parse(fixture.resource);
  const groups = groupResourceUsage(resource.processes);

  expect(formatPercent(null)).toBe("—");
  expect(groups.find((group) => group.category === "base")?.cpuPercent).toBeNull();
});
