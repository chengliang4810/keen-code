import { createHash } from "node:crypto";
import { readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { inspectSource, DESIGN_BASELINE_MANIFEST } from "./design-system-gate.mjs";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(scriptDir, "../..");
const sourceRoot = path.resolve(process.argv[2] ?? path.join(repoRoot, "..", "ZCode"), "packages/ui/src");
const commit = process.argv[3] ?? "29628c9acdb81b703bbd4080c207a0e7ce5e276e";
const supportedExtensions = new Set([".css", ".jsx", ".tsx"]);

function sha256(content) {
  return createHash("sha256").update(content, "utf8").digest("hex");
}

async function listFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const absolute = path.join(directory, entry.name);
    if (entry.isDirectory()) files.push(...await listFiles(absolute));
    else if (supportedExtensions.has(path.extname(entry.name).toLowerCase())) files.push(absolute);
  }
  return files;
}

const files = {};
for (const absolute of await listFiles(sourceRoot)) {
  const content = await readFile(absolute, "utf8");
  const relative = `packages/ui/src/${path.relative(sourceRoot, absolute).replaceAll("\\", "/")}`;
  const violations = inspectSource(relative, content);
  if (violations.length === 0) continue;

  const lineFeatures = {};
  for (const violation of violations) {
    const bucket = lineFeatures[violation.rule] ?? [];
    bucket.push({
      line: violation.line,
      lineHash: violation.lineHash,
      featureHash: violation.featureHash,
    });
    lineFeatures[violation.rule] = bucket;
  }
  files[relative] = {
    sha256: sha256(content),
    rules: Object.keys(lineFeatures).sort(),
    lineFeatures,
  };
}

const manifest = {
  schemaVersion: 1,
  source: "ZCode 3.14.3",
  commit,
  sourceRoot: "packages/ui/src",
  rule: "Only unchanged source files or exact unchanged line features may inherit baseline violations.",
  files,
};

await writeFile(DESIGN_BASELINE_MANIFEST, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
console.log(`Generated ${Object.keys(files).length} source-file baseline entries at ${path.relative(repoRoot, DESIGN_BASELINE_MANIFEST)}.`);
