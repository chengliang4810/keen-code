import { createRequire } from "node:module";

export function createCatppuccinAssets(iconSet) {
  const width = iconSet.width ?? 16;
  const height = iconSet.height ?? 16;
  const svgs = new Map(
    Object.entries(iconSet.icons).map(([name, icon]) => [
      name,
      `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}">${icon.body}</svg>`,
    ]),
  );
  const aliases = new Map();
  for (const [name, alias] of Object.entries(iconSet.aliases ?? {})) {
    if (svgs.has(alias.parent)) aliases.set(name, alias.parent);
  }
  return { svgs, aliases };
}

export function catppuccinAssetsPlugin() {
  const require = createRequire(import.meta.url);
  const { svgs, aliases } = createCatppuccinAssets(
    require("@iconify-json/catppuccin/icons.json"),
  );
  const publicId = "virtual:rcode-catppuccin-icons";
  const resolvedId = `\0${publicId}`;
  let build = false;
  let route = "/@rcode-catppuccin/";
  return {
    name: "rcode-catppuccin-assets",
    configResolved(config) {
      build = config.command === "build";
      const base = new URL(config.base, "http://localhost").pathname;
      route = `${base.endsWith("/") ? base : `${base}/`}@rcode-catppuccin/`;
    },
    resolveId(id) {
      if (id === publicId) return resolvedId;
    },
    load(id) {
      if (id !== resolvedId) return;
      const values = new Map();
      for (const [name, source] of svgs) {
        const url = build
          ? `import.meta.ROLLUP_FILE_URL_${this.emitFile({ type: "asset", name: `catppuccin-${name}.svg`, source })}`
          : JSON.stringify(`${route}${encodeURIComponent(name)}.svg`);
        values.set(name, url);
      }
      for (const [name, parent] of aliases)
        values.set(name, values.get(parent));
      return `export default {${[...values].map(([name, url]) => `${JSON.stringify(name)}:${url}`).join(",")}};`;
    },
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const pathname = new URL(request.url ?? "/", "http://localhost")
          .pathname;
        if (!pathname.startsWith(route) || !pathname.endsWith(".svg"))
          return next();
        const source = svgs.get(
          decodeURIComponent(pathname.slice(route.length, -4)),
        );
        if (source === undefined) return next();
        response.setHeader("Content-Type", "image/svg+xml");
        response.end(source);
      });
    },
  };
}
