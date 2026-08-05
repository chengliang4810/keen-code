import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  buildReleaseMetadata,
  finalizeUpdaterManifest,
  writeTauriReleaseConfig,
} from "./release-version.mjs";

test("uses the commit date in China Standard Time and a short SHA tag", () => {
  const metadata = buildReleaseMetadata({
    sha: "49ad19b1234567890abcdef1234567890abcdef1",
    commitTimestamp: Date.parse("2026-07-30T14:35:20+08:00") / 1000,
  });

  assert.deepEqual(metadata, {
    tag: "v20260730-49ad19b",
    appVersion: "2026.730.26260",
    releaseName: "KeenCode v20260730-49ad19b",
    date: "20260730",
  });
});

test("keeps the internal package version within native platform limits", () => {
  const metadata = buildReleaseMetadata({
    sha: "abcdef0123456789",
    commitTimestamp: Date.parse("2026-12-31T23:59:59+08:00") / 1000,
  });
  const parts = metadata.appVersion.split(".").map(Number);

  assert.equal(parts.length, 3);
  assert.ok(parts.every((part) => part >= 0 && part <= 65_535));
});

test("writes only the release overrides used by CI", async () => {
  const directory = await mkdtemp(join(tmpdir(), "keencode-release-"));
  const path = join(directory, "tauri.release.conf.json");
  try {
    writeTauriReleaseConfig(path, "2026.805.1234");
    assert.deepEqual(JSON.parse(readFileSync(path, "utf8")), {
      version: "2026.805.1234",
      bundle: { createUpdaterArtifacts: true },
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("validates all updater targets and writes the public release tag", async () => {
  const directory = await mkdtemp(join(tmpdir(), "keencode-updater-"));
  const path = join(directory, "latest.json");
  const target = (name) => ({
    signature: `${name}-signature`,
    url: `https://api.github.com/assets/${name}`,
  });
  try {
    writeFileSync(
      path,
      JSON.stringify({
        version: "2026.805.1234",
        platforms: {
          "darwin-aarch64": target("mac-arm"),
          "darwin-x86_64": target("mac-intel"),
          "windows-x86_64": target("windows"),
        },
      }),
    );

    finalizeUpdaterManifest(path, "v20260805-abcdef0");

    assert.equal(
      JSON.parse(readFileSync(path, "utf8")).release,
      "v20260805-abcdef0",
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("refuses to publish an updater manifest missing a platform", async () => {
  const directory = await mkdtemp(join(tmpdir(), "keencode-updater-"));
  const path = join(directory, "latest.json");
  try {
    writeFileSync(path, JSON.stringify({ platforms: {} }));
    assert.throws(
      () => finalizeUpdaterManifest(path, "v20260805-abcdef0"),
      /darwin-aarch64/,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
