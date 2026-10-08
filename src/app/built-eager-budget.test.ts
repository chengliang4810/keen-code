import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { gzipSync } from "node:zlib";
import { afterEach, describe, expect, it } from "vitest";
import {
  builtHeavyHits,
  staticValueSpecifiers,
  traceBuiltEager,
} from "../../scripts/built-eager-graph.mjs";

const directories: string[] = [];
function fixture(files: Record<string, string>) {
  const root = mkdtempSync(join(tmpdir(), "rcode-bundle-budget-"));
  directories.push(root);
  mkdirSync(join(root, "assets"));
  for (const [file, code] of Object.entries(files)) {
    mkdirSync(dirname(join(root, file)), { recursive: true });
    writeFileSync(join(root, file), code);
  }
  return root;
}
afterEach(() => {
  for (const root of directories.splice(0))
    rmSync(root, { recursive: true, force: true });
});

describe("built startup graph", () => {
  it("identifies SDK ownership when a shared chunk has no provider name", () => {
    const root = fixture({
      "index.html": '<script type="module" src="/assets/main.js"></script>',
      "assets/main.js": 'import "./shared.js"; import("./lazy.js");',
      "assets/shared.js": "export const value = 1;",
      "assets/lazy.js": "export const value = 2;",
      ".vite/rcode-bundle-graph.json": JSON.stringify([
        {
          file: "assets/main.js",
          sha256: createHash("sha256")
            .update('import "./shared.js"; import("./lazy.js");')
            .digest("hex"),
          modules: [],
        },
        {
          file: "assets/shared.js",
          sha256: createHash("sha256")
            .update("export const value = 1;")
            .digest("hex"),
          modules: [
            "node_modules/.pnpm/provider/node_modules/@ai-sdk/anthropic/dist/index.mjs",
          ],
        },
        {
          file: "assets/lazy.js",
          modules: [
            "node_modules/.pnpm/editor/node_modules/@codemirror/view/dist/index.js",
          ],
        },
      ]),
    });
    expect(builtHeavyHits(root, traceBuiltEager(root).files)).toEqual([
      {
        chunk: "assets/shared.js",
        module:
          "node_modules/.pnpm/provider/node_modules/@ai-sdk/anthropic/dist/index.mjs",
      },
    ]);
  });
  it("rejects missing ownership and stale build evidence", () => {
    const root = fixture({
      "index.html": '<script type="module" src="/assets/main.js"></script>',
      "assets/main.js": "export const value = 1;",
      ".vite/rcode-bundle-graph.json": "[]",
    });
    const startup = traceBuiltEager(root);
    expect(() => builtHeavyHits(root, startup.files)).toThrow(
      "Startup chunk missing",
    );
    writeFileSync(
      join(root, ".vite/rcode-bundle-graph.json"),
      JSON.stringify([
        { file: "assets/main.js", sha256: "stale", modules: [] },
      ]),
    );
    expect(() => builtHeavyHits(root, startup.files)).toThrow(
      "Stale bundle graph",
    );
  });
  it("counts shared wrappers and independent preloads without counting lazy chunks", () => {
    const files = {
      "index.html":
        '<script src="/assets/main.js" type="module"></script><link href="/assets/preload.js" rel="modulepreload">',
      "assets/main.js":
        'import { value } from "./shared.js"; import("./lazy.js"); console.log(value);',
      "assets/shared.js": 'export { value } from "./provider.js";',
      "assets/provider.js": 'export const value = "provider";',
      "assets/preload.js": 'import "./shared.js";',
      "assets/lazy.js": 'console.log("lazy");',
    };
    const result = traceBuiltEager(fixture(files));
    expect(result.files.map((file) => basename(file)).sort()).toEqual([
      "main.js",
      "preload.js",
      "provider.js",
      "shared.js",
    ]);
    expect(result.gzipBytes).toBe(
      ["main", "preload", "provider", "shared"].reduce(
        (sum, name) =>
          sum +
          gzipSync(files[`assets/${name}.js` as keyof typeof files], {
            level: 9,
          }).length,
        0,
      ),
    );
  });

  it("fails on a missing transitive chunk instead of understating startup", () => {
    const root = fixture({
      "index.html": '<script type="module" src="assets/main.js"></script>',
      "assets/main.js": 'import "./missing.js";',
    });
    expect(() => traceBuiltEager(root)).toThrow();
  });

  it("fails if an external module cannot be measured", () => {
    const root = fixture({
      "index.html":
        '<script type="module" src="https://example.test/main.js"></script>',
    });
    expect(() => traceBuiltEager(root)).toThrow("external startup dependency");
  });

  it("traces minified static imports and excludes all erased type imports", () => {
    expect(
      staticValueSpecifiers(
        'import{value}from"runtime";export{value}from"shared";import("lazy");',
      ),
    ).toEqual(["runtime", "shared"]);
    expect(
      staticValueSpecifiers(
        'import type { Foo } from "a";import { type Bar } from "b";export type { Baz } from "c";export { type Qux } from "d";',
        "module.ts",
      ),
    ).toEqual([]);
    expect(
      staticValueSpecifiers(
        'import { type Foo, value } from "runtime";',
        "module.ts",
      ),
    ).toEqual(["runtime"]);
  });
});
