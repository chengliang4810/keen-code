import assert from "node:assert/strict";
import { mkdtemp, readFile, readdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { serializedJsonWriter, validateManifest } from "./benchmark-batch.mjs";

const valid = () => ({
  outputDirectory: "/tmp/keencode-results",
  defaults: { model: "model", baseUrl: "https://example.invalid/v1" },
  tasks: [{ id: "task-1", cwd: "/tmp/project", prompts: ["fix it"] }],
});

test("defaults to the provider concurrency limit", () => {
  assert.equal(validateManifest(valid()).concurrency, 6);
});

test("rejects unsafe task ids, duplicate ids and excess concurrency", () => {
  assert.throws(() => validateManifest({ ...valid(), concurrency: 7 }), /1-6/);
  assert.throws(() => validateManifest({ ...valid(), tasks: [{ id: "../escape", cwd: "/tmp/p", prompts: ["x"] }] }), /id/);
  const task = valid().tasks[0];
  assert.throws(() => validateManifest({ ...valid(), tasks: [task, task] }), /重复/);
});

test("requires absolute output and task paths", () => {
  assert.throws(() => validateManifest({ ...valid(), outputDirectory: "relative" }), /绝对/);
  assert.throws(() => validateManifest({ ...valid(), tasks: [{ id: "x", cwd: "relative", prompts: ["x"] }] }), /绝对/);
});

test("serializes concurrent summary snapshots without temporary residue", async () => {
  const directory = await mkdtemp(join(tmpdir(), "keencode-batch-writer-"));
  const path = join(directory, "summary.json");
  const write = serializedJsonWriter(path);
  await Promise.all(Array.from({ length: 24 }, (_, index) => write({ index, tasks: Array(index).fill("done") })));
  assert.deepEqual(JSON.parse(await readFile(path, "utf8")), {
    index: 23,
    tasks: Array(23).fill("done"),
  });
  assert.deepEqual(await readdir(directory), ["summary.json"]);
});
