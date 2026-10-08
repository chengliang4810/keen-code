import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, isAbsolute, relative, resolve } from "node:path";
import { gzipSync } from "node:zlib";
import ts from "typescript";

export function staticValueSpecifiers(code, file = "module.js") {
  const source = ts.createSourceFile(file, code, ts.ScriptTarget.Latest, true);
  const specs = new Set();
  for (const statement of source.statements) {
    if (ts.isImportDeclaration(statement)) {
      const clause = statement.importClause;
      if (clause?.isTypeOnly) continue;
      if (
        clause &&
        !clause.name &&
        clause.namedBindings &&
        ts.isNamedImports(clause.namedBindings) &&
        clause.namedBindings.elements.length > 0 &&
        clause.namedBindings.elements.every((element) => element.isTypeOnly)
      )
        continue;
    } else if (ts.isExportDeclaration(statement)) {
      if (statement.isTypeOnly) continue;
      if (
        statement.exportClause &&
        ts.isNamedExports(statement.exportClause) &&
        statement.exportClause.elements.length > 0 &&
        statement.exportClause.elements.every((element) => element.isTypeOnly)
      )
        continue;
    } else continue;
    if (
      statement.moduleSpecifier &&
      ts.isStringLiteralLike(statement.moduleSpecifier)
    ) {
      specs.add(statement.moduleSpecifier.text);
    }
  }
  return [...specs];
}

function localAsset(spec, from, root) {
  if (/^(?:[a-z]+:|\/\/)/i.test(spec)) {
    throw new Error(`Cannot measure external startup dependency: ${spec}`);
  }
  const pathname = decodeURIComponent(spec.split(/[?#]/, 1)[0]);
  const file = pathname.startsWith("/")
    ? resolve(root, pathname.slice(1))
    : resolve(dirname(from), pathname);
  const location = relative(root, file);
  if (location.startsWith("..") || isAbsolute(location)) {
    throw new Error(`Startup dependency leaves the build directory: ${spec}`);
  }
  return file;
}

function attribute(tag, name) {
  return tag.match(new RegExp(`\\b${name}\\s*=\\s*(["'])(.*?)\\1`, "i"))?.[2];
}

export function traceBuiltEager(buildDirectory, htmlFile = "index.html") {
  const root = resolve(buildDirectory);
  const entry = resolve(root, htmlFile);
  const html = readFileSync(entry, "utf8");
  const seeds = [];
  for (const match of html.matchAll(/<(?:script|link)\b[^>]*>/gi)) {
    const tag = match[0];
    const spec = /^<script/i.test(tag)
      ? attribute(tag, "type") === "module" && attribute(tag, "src")
      : attribute(tag, "rel") === "modulepreload" && attribute(tag, "href");
    if (spec) seeds.push(localAsset(spec, entry, root));
  }
  if (seeds.length === 0)
    throw new Error(`No startup modules found in ${entry}`);
  const assets = new Map();
  const queue = [...seeds];
  for (let index = 0; index < queue.length; index++) {
    const file = queue[index];
    if (assets.has(file)) continue;
    const bytes = readFileSync(file);
    const dependencies = staticValueSpecifiers(
      bytes.toString("utf8"),
      file,
    ).map((spec) => localAsset(spec, file, root));
    assets.set(file, {
      path: file,
      bytes: bytes.length,
      gzipBytes: gzipSync(bytes, { level: 9 }).length,
      dependencies,
    });
    queue.push(...dependencies);
  }
  return {
    files: [...assets.keys()],
    rawBytes: [...assets.values()].reduce((sum, asset) => sum + asset.bytes, 0),
    gzipBytes: [...assets.values()].reduce(
      (sum, asset) => sum + asset.gzipBytes,
      0,
    ),
    assets,
  };
}

export function builtHeavyHits(buildDirectory, startupFiles) {
  const root = resolve(buildDirectory);
  const graph = JSON.parse(
    readFileSync(resolve(root, ".vite/rcode-bundle-graph.json"), "utf8"),
  );
  const reachable = new Set(
    startupFiles.map((file) => relative(root, file).replaceAll("\\", "/")),
  );
  for (const file of reachable) {
    const chunk = graph.find((entry) => entry.file === file);
    if (!chunk || !Array.isArray(chunk.modules)) {
      throw new Error(`Startup chunk missing from the bundle graph: ${file}`);
    }
    const actual = createHash("sha256")
      .update(readFileSync(resolve(root, file)))
      .digest("hex");
    if (chunk.sha256 !== actual)
      throw new Error(`Stale bundle graph for startup chunk: ${file}`);
  }
  return graph
    .filter((chunk) => reachable.has(chunk.file))
    .flatMap((chunk) =>
      chunk.modules
        .filter((id) =>
          /(?:\/node_modules\/(?:@ai-sdk\/|ai\/|streamdown\/|@codemirror\/|@uiw\/)|\/terminal\/ghostty\/webgl\/)/.test(
            id,
          ),
        )
        .map((module) => ({ chunk: chunk.file, module })),
    );
}
