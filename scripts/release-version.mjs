import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const CHINA_STANDARD_TIME_OFFSET_SECONDS = 8 * 60 * 60;
const WINDOWS_MAX_MAJOR = 255;
const WINDOWS_MAX_MINOR = 255;
const WINDOWS_MAX_PATCH = 65_535;
const VERSION_SLOTS_PER_MAJOR =
  (WINDOWS_MAX_MINOR + 1) * (WINDOWS_MAX_PATCH + 1);
const MAX_RELEASE_COMMIT_COUNT = WINDOWS_MAX_MAJOR * VERSION_SLOTS_PER_MAJOR;
const REQUIRED_UPDATER_TARGETS = [
  "darwin-aarch64",
  "darwin-x86_64",
  "windows-x86_64",
];

function pad2(value) {
  return String(value).padStart(2, "0");
}

/**
 * Build the public tag and a monotonic native package version from one
 * immutable commit. MSI limits major/minor to 255 and patch to 65535.
 */
export function buildReleaseMetadata({ sha, commitTimestamp, commitCount }) {
  if (!/^[0-9a-f]{7,40}$/i.test(sha)) {
    throw new Error("release SHA must contain 7-40 hexadecimal characters");
  }
  if (!Number.isSafeInteger(commitTimestamp) || commitTimestamp <= 0) {
    throw new Error("commit timestamp must be a positive Unix timestamp");
  }
  if (
    !Number.isSafeInteger(commitCount) ||
    commitCount <= 0 ||
    commitCount > MAX_RELEASE_COMMIT_COUNT
  ) {
    throw new Error("release commit count exceeds native version capacity");
  }

  const chinaTime = new Date(
    (commitTimestamp + CHINA_STANDARD_TIME_OFFSET_SECONDS) * 1000,
  );
  const year = chinaTime.getUTCFullYear();
  const month = chinaTime.getUTCMonth() + 1;
  const day = chinaTime.getUTCDate();
  const date = `${year}${pad2(month)}${pad2(day)}`;
  const shortSha = sha.slice(0, 7).toLowerCase();
  const tag = `v${date}-${shortSha}`;
  const versionOrdinal = commitCount - 1;
  const versionRemainder = versionOrdinal % VERSION_SLOTS_PER_MAJOR;
  const major = Math.floor(versionOrdinal / VERSION_SLOTS_PER_MAJOR) + 1;
  const minor = Math.floor(versionRemainder / (WINDOWS_MAX_PATCH + 1));
  const patch = versionRemainder % (WINDOWS_MAX_PATCH + 1);

  return {
    tag,
    appVersion: `${major}.${minor}.${patch}`,
    releaseName: `KeenCode ${tag}`,
    date,
  };
}

export function writeTauriReleaseConfig(path, appVersion) {
  if (!/^\d+\.\d+\.\d+$/.test(appVersion)) {
    throw new Error("release package version must have three numeric parts");
  }
  writeFileSync(
    path,
    `${JSON.stringify(
      {
        version: appVersion,
        bundle: { createUpdaterArtifacts: true },
      },
      null,
      2,
    )}\n`,
  );
}

/**
 * 发布前将 GitHub Asset API 地址替换为匿名可下载地址，并校验三平台更新条目。
 * API 地址受匿名请求限额约束，客户端必须使用 release download 地址。
 */
export function finalizeUpdaterManifest(
  path,
  releaseTag,
  repository,
  releaseAssets,
) {
  if (!/^v\d{8}-[0-9a-f]{7}$/i.test(releaseTag)) {
    throw new Error("release tag must match vYYYYMMDD-abcdef0");
  }
  if (!/^[a-z0-9_.-]+\/[a-z0-9_.-]+$/i.test(repository)) {
    throw new Error("GitHub repository must use the owner/name format");
  }
  const manifest = JSON.parse(readFileSync(path, "utf8"));
  const assets = releaseAssets?.assets;
  if (!Array.isArray(assets) || assets.length === 0) {
    throw new Error("release assets are required to finalize updater URLs");
  }
  const publicUrlByAssetUrl = new Map();
  for (const asset of assets) {
    if (
      typeof asset?.apiUrl !== "string" ||
      typeof asset?.name !== "string" ||
      asset.name.length === 0 ||
      /[\\/]/.test(asset.name)
    ) {
      throw new Error("release asset metadata contains an invalid asset");
    }
    const publicUrl = `https://github.com/${repository}/releases/download/${releaseTag}/${encodeURIComponent(asset.name)}`;
    publicUrlByAssetUrl.set(asset.apiUrl, publicUrl);
    publicUrlByAssetUrl.set(publicUrl, publicUrl);
    if (typeof asset.url === "string") {
      publicUrlByAssetUrl.set(asset.url, publicUrl);
    }
  }

  for (const [target, entry] of Object.entries(manifest.platforms ?? {})) {
    if (typeof entry !== "object" || entry === null) {
      throw new Error(`updater manifest contains an invalid ${target} target`);
    }
    const publicUrl = publicUrlByAssetUrl.get(entry.url);
    if (!publicUrl) {
      throw new Error(`updater manifest ${target} URL does not match a release asset`);
    }
    entry.url = publicUrl;
  }

  for (const target of REQUIRED_UPDATER_TARGETS) {
    const entry = manifest.platforms?.[target];
    if (
      typeof entry !== "object" ||
      entry === null ||
      typeof entry.signature !== "string" ||
      entry.signature.length === 0 ||
      typeof entry.url !== "string" ||
      entry.url.length === 0
    ) {
      throw new Error(`updater manifest is missing a valid ${target} target`);
    }
  }
  manifest.release = releaseTag;
  writeFileSync(path, `${JSON.stringify(manifest, null, 2)}\n`);
}

function gitValue(args) {
  return execFileSync("git", args, { encoding: "utf8" }).trim();
}

function writeGithubOutputs(metadata, outputPath) {
  const lines = [
    `tag=${metadata.tag}`,
    `app_version=${metadata.appVersion}`,
    `release_name=${metadata.releaseName}`,
    `date=${metadata.date}`,
  ];
  appendFileSync(outputPath, `${lines.join("\n")}\n`);
}

function main() {
  const [command, path, value] = process.argv.slice(2);
  if (command === "--write-tauri-config") {
    if (!path || !value) {
      throw new Error("usage: --write-tauri-config <path> <app-version>");
    }
    writeTauriReleaseConfig(path, value);
    return;
  }
  if (command === "--finalize-updater-manifest") {
    const assetsPath = process.argv[5];
    const repository = process.argv[6];
    if (!path || !value || !assetsPath || !repository) {
      throw new Error(
        "usage: --finalize-updater-manifest <path> <release-tag> <release-assets> <repository>",
      );
    }
    finalizeUpdaterManifest(
      path,
      value,
      repository,
      JSON.parse(readFileSync(assetsPath, "utf8")),
    );
    return;
  }

  const sha = process.env.KEENCODE_COMMIT_SHA || gitValue(["rev-parse", "HEAD"]);
  const commitTimestamp = Number(
    process.env.KEENCODE_COMMIT_TIMESTAMP ||
      gitValue(["show", "-s", "--format=%ct", sha]),
  );
  const commitCount = Number(
    process.env.KEENCODE_COMMIT_COUNT ||
      gitValue(["rev-list", "--first-parent", "--count", sha]),
  );
  const metadata = buildReleaseMetadata({ sha, commitTimestamp, commitCount });
  if (process.env.GITHUB_OUTPUT) {
    writeGithubOutputs(metadata, process.env.GITHUB_OUTPUT);
  }
  process.stdout.write(`${JSON.stringify(metadata)}\n`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}
