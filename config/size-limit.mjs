import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { relative } from "node:path";
import { traceBuiltEager } from "../scripts/built-eager-graph.mjs";

const checks = JSON.parse(
  readFileSync(new URL("size-limit.json", import.meta.url), "utf8"),
);
const startup = traceBuiltEager(
  fileURLToPath(new URL("../apps/desktop/dist/", import.meta.url)),
);

export default checks.map((check, index) =>
  index === 0
    ? {
        ...check,
        path: startup.files.map((file) =>
          relative(
            fileURLToPath(new URL(".", import.meta.url)),
            file,
          ).replaceAll("\\", "/"),
        ),
      }
    : check,
);
