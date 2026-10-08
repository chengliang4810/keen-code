import { fileURLToPath } from "node:url";
import { builtHeavyHits, traceBuiltEager } from "./built-eager-graph.mjs";

const directory =
  process.argv[2] ?? fileURLToPath(new URL("../apps/desktop/dist/", import.meta.url));
const startup = traceBuiltEager(directory);
const heavyStacks = builtHeavyHits(directory, startup.files);
console.log(
  JSON.stringify(
    {
      startupFiles: startup.files.length,
      startupRawBytes: startup.rawBytes,
      startupGzipBytes: startup.gzipBytes,
      heavyStacks,
    },
    null,
    2,
  ),
);
if (heavyStacks.length > 0) {
  throw new Error(
    "AI, editor, Markdown and WebGL stacks must load through their dynamic imports.",
  );
}
