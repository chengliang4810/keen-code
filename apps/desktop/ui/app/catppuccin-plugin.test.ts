import icons from "@iconify-json/catppuccin/icons.json";
import { build, createServer } from "vite";
import { describe, expect, it } from "vitest";
import { catppuccinAssetsPlugin } from "../../../../scripts/catppuccin-assets.mjs";

describe("Catppuccin resource URLs", () => {
  it.each(["/nested/", "./"])(
    "emits all SVGs through Vite asset URLs with base %s",
    async (base) => {
      const result = await build({
        configFile: false,
        logLevel: "silent",
        base,
        plugins: [
          catppuccinAssetsPlugin(),
          {
            name: "icon-resource-test-entry",
            resolveId(id) {
              if (id === "icon-resource-test-entry") return `\0${id}`;
            },
            load(id) {
              if (id === "\0icon-resource-test-entry")
                return 'export { default as urls } from "virtual:rcode-catppuccin-icons";';
            },
          },
        ],
        build: {
          write: false,
          minify: false,
          rolldownOptions: {
            input: "icon-resource-test-entry",
            preserveEntrySignatures: "strict",
          },
        },
      });
      if (Array.isArray(result) || !("output" in result))
        throw new Error("Expected one resource build");
      const assets = result.output.filter((output) => output.type === "asset");
      const sources = new Set(assets.map((asset) => String(asset.source)));
      for (const [name, icon] of Object.entries(icons.icons)) {
        expect(
          sources.has(
            `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">${icon.body}</svg>`,
          ),
          name,
        ).toBe(true);
      }
      const chunk = result.output.find((output) => output.type === "chunk");
      expect(chunk?.code).toContain("catppuccin-typescript");
      if (base === "/nested/")
        expect(chunk?.code).toContain("/nested/assets/catppuccin-");
      else expect(chunk?.code).toContain("import.meta.url");
    },
  );

  it("serves the exact SVG under a non-root development base", async () => {
    const server = await createServer({
      configFile: false,
      logLevel: "silent",
      base: "/nested/",
      plugins: [catppuccinAssetsPlugin()],
      server: { host: "127.0.0.1", port: 0 },
    });
    try {
      await server.listen();
      const address = server.httpServer?.address();
      if (!address || typeof address === "string")
        throw new Error("Development server address missing");
      const response = await fetch(
        `http://127.0.0.1:${address.port}/nested/@rcode-catppuccin/typescript.svg`,
      );
      expect(response.status).toBe(200);
      expect(response.headers.get("Content-Type")).toContain("image/svg+xml");
      expect(await response.text()).toBe(
        `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">${icons.icons.typescript.body}</svg>`,
      );
    } finally {
      await server.close();
    }
  });
});
