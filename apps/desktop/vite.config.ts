import babel from "@rolldown/plugin-babel";
import tailwindcss from "@tailwindcss/vite";
import react, { reactCompilerPreset } from "@vitejs/plugin-react";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import {
  defineConfig,
  type Plugin,
  type PluginOption,
  type UserConfig,
} from "vite";
import Inspect from "vite-plugin-inspect";
import { catppuccinAssetsPlugin } from "../../scripts/catppuccin-assets.mjs";

const host = process.env.TAURI_DEV_HOST;
const rootDir = import.meta.dirname;
const require = createRequire(import.meta.url);

function dependencyEntry(importer: string, dependency: string): string {
  const entry = createRequire(require.resolve(importer)).resolve(dependency);
  let directory = path.dirname(entry);
  while (directory !== path.dirname(directory)) {
    try {
      const manifest = JSON.parse(
        readFileSync(path.join(directory, "package.json"), "utf8"),
      );
      if (manifest.name === dependency)
        return manifest.module
          ? path.resolve(directory, manifest.module)
          : entry;
    } catch {}
    directory = path.dirname(directory);
  }
  throw new Error(`Cannot resolve ${dependency} from ${importer}`);
}

const bundleGraph: Plugin = {
  name: "rcode-bundle-graph",
  apply: "build",
  generateBundle: {
    order: "post",
    handler(_options, bundle) {
      const chunks = Object.values(bundle).filter(
        (output) => output.type === "chunk",
      );
      this.emitFile({
        type: "asset",
        fileName: ".vite/rcode-bundle-graph.json",
        source: JSON.stringify(
          chunks.map((chunk) => ({
            file: chunk.fileName,
            sha256: createHash("sha256").update(chunk.code).digest("hex"),
            imports: chunk.imports,
            dynamicImports: chunk.dynamicImports,
            modules: Object.keys(chunk.modules).map((id) =>
              path.relative(rootDir, id).replaceAll("\\", "/"),
            ),
          })),
        ),
      });
    },
  },
};

// Bundle/treemap analysis is opt-in: `ANALYZE=true pnpm build` emits stats.html.
const analyze = process.env.ANALYZE === "true";

// Module-graph inspector is opt-in via `pnpm dev:inspect`; keeps plain
// `pnpm dev` from paying its transform-tracking overhead on every run.
const inspectGraph = process.env.INSPECT === "true";

// https://vite.dev/config/
export default defineConfig(
  async ({ mode }): Promise<UserConfig> => ({
    plugins: [
      babel({
        presets: [reactCompilerPreset({ target: "19" })],
      }),
      react(),
      tailwindcss(),
      bundleGraph,
      catppuccinAssetsPlugin(),
      // Module-graph inspector at /__inspect (who-imports-what, per-plugin
      // transforms). Opt-in via `pnpm dev:inspect`, never in a production build.
      ...(mode === "development" && inspectGraph
        ? [Inspect() as PluginOption]
        : []),
      ...(analyze
        ? [
            (await import("rollup-plugin-visualizer")).visualizer({
              filename: "stats.html",
              template: "treemap",
              gzipSize: true,
              brotliSize: true,
              open: true,
            }) as PluginOption,
          ]
        : []),
    ],
    resolve: {
      alias: {
        "@": path.resolve(rootDir, "./ui"),
        "@radix-ui/react-dialog": dependencyEntry(
          "radix-ui",
          "@radix-ui/react-dialog",
        ),
        "@radix-ui/react-primitive": dependencyEntry(
          "radix-ui",
          "@radix-ui/react-primitive",
        ),
        crelt: dependencyEntry("@codemirror/view", "crelt"),
        "style-mod": dependencyEntry("@codemirror/view", "style-mod"),
        // Shim keeps the ~117 kB CJS protocol package out of the bundle.
        "vscode-languageserver-protocol": path.resolve(
          rootDir,
          "./ui/modules/lsp/lib/protocolShim.ts",
        ),
      },
      dedupe: ["@lezer/highlight"],
    },
    build: {
      target:
        process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome120" : "es2022",
      chunkSizeWarningLimit: 1500,
      rolldownOptions: {
        input: {
          main: path.resolve(rootDir, "index.html"),
        },
        // Oxc drops `debugger` by default. These calls return undefined, so
        // marking them pure lets DCE strip them from production builds.
        treeshake: {
          manualPureFunctions: [
            "console.debug",
            "console.info",
            "console.trace",
          ],
        },
        output: {
          codeSplitting: {
            groups: [
              {
                name: "startup",
                priority: 110,
                tags: ["$initial"],
                test: (id: string) =>
                  !/\/(?:packages\/ghostty-core|ui\/modules\/terminal\/ghostty)\//.test(
                    id,
                  ),
                includeDependenciesRecursively: false,
              },
              {
                name: "react",
                priority: 120,
                // The terminal reads these values while its module initializes.
                test: /(?:vite\/preload-helper|\/vite\/dist\/|node_modules\/(?:react|react-dom|scheduler|clsx|tailwind-merge|class-variance-authority)\/|\/ui\/modules\/terminal\/lib\/LatestClipboardWrite\.ts$)/,
              },
              {
                name: "radix",
                priority: 90,
                test: /(?:@radix-ui\/|\/radix-ui\/)/,
              },
              {
                name: "ai-sdk-shared",
                priority: 80,
                test: /@ai-sdk\/(?:provider(?:-utils)?|react)\//,
              },
              {
                name: "codemirror",
                priority: 80,
                test: /(?:@codemirror\/(?!lang-|legacy-modes)|@uiw\/codemirror|@replit\/codemirror(?!-lang)|@lezer\/(?:common|lr|highlight)\/)/,
              },
              {
                name: "icons",
                priority: 70,
                test: /@hugeicons\/core-free-icons\//,
                includeDependenciesRecursively: false,
              },
              {
                debugName: "features",
                name(id: string) {
                  // Preload helpers must not pull a lazy feature into startup.
                  if (
                    id.includes("vite/preload-helper") ||
                    id.includes("/vite/dist/")
                  )
                    return "react";

                  if (id.includes("/packages/ghostty-core/"))
                    return "ghostty-vt";

                  if (!id.includes("node_modules")) return null;

                  // Shared styling helpers belong to the eager application shell.
                  if (
                    id.includes("/clsx/") ||
                    id.includes("/tailwind-merge/") ||
                    id.includes("/class-variance-authority/")
                  )
                    return "react";

                  // Each AI provider SDK in its own chunk so unused providers
                  // don't bloat the initial load (lazy-imported in agent.ts).
                  if (id.includes("@ai-sdk/anthropic")) return "ai-anthropic";
                  if (id.includes("@ai-sdk/google")) return "ai-google";
                  if (id.includes("@ai-sdk/openai-compatible"))
                    return "ai-openai-compat";
                  if (id.includes("@ai-sdk/openai")) return "ai-openai";
                  if (id.includes("@ai-sdk/cerebras")) return "ai-cerebras";
                  if (id.includes("@ai-sdk/groq")) return "ai-groq";
                  if (id.includes("@ai-sdk/xai")) return "ai-xai";
                  if (id.includes("@ai-sdk/")) return "ai-sdk-shared";

                  // Each language grammar retains its own lazy loading boundary.
                  {
                    const m = id.match(/@codemirror\/lang-([\w-]+)/);
                    if (m) return `cm-lang-${m[1]}`;
                  }
                  {
                    const m = id.match(
                      /@codemirror\/legacy-modes\/mode\/([\w-]+)/,
                    );
                    if (m) return `cm-legacy-${m[1]}`;
                  }
                  if (id.includes("@replit/codemirror-lang-svelte"))
                    return "cm-lang-svelte";
                  if (
                    id.includes("@codemirror/") ||
                    id.includes("@uiw/codemirror") ||
                    id.includes("@replit/codemirror")
                  )
                    return "codemirror";
                  if (
                    id.includes("/streamdown/") ||
                    id.includes("@streamdown/")
                  )
                    return "streamdown";
                  if (
                    id.includes("/react-dom/") ||
                    id.includes("/react/") ||
                    id.includes("/scheduler/")
                  )
                    return "react";
                  if (id.includes("@radix-ui/") || id.includes("/radix-ui/"))
                    return "radix";

                  return null;
                },
              },
            ],
          },
        },
      },
    },
    clearScreen: false,
    server: {
      port: 1420,
      strictPort: true,
      host: host || false,
      hmr: host
        ? {
            protocol: "ws",
            host,
            port: 1421,
          }
        : undefined,
      watch: {
        ignored: ["**/apps/desktop/src/**"],
      },
    },
  }),
);
