import type {
  AvailablePlugin,
  InstalledPlugin,
  PluginConfigField,
} from "@/modules/ai/lib/extensions";

export function installedPluginId(plugin: InstalledPlugin): string {
  return `${plugin.id.plugin}@${plugin.id.marketplace ?? "local"}`;
}

export function marketplaceSourceDirectory(source: string): string | null {
  if (!/^(?:[a-z]:[\\/]|\/|\\\\)/i.test(source)) return null;
  const normalized = source.replace(/\\/g, "/");
  if (!/\.json$/i.test(normalized)) return normalized;
  const separator = normalized.lastIndexOf("/");
  return normalized.slice(
    0,
    separator === 0 || (separator === 2 && normalized[1] === ":")
      ? separator + 1
      : separator,
  );
}

export function filterMarketplace(
  plugins: AvailablePlugin[],
  query: string,
  market: string,
): AvailablePlugin[] {
  const search = query.trim().toLocaleLowerCase();
  return plugins.filter(
    (plugin) =>
      (!market || plugin.marketplace === market) &&
      (!search ||
        [
          plugin.name,
          plugin.marketplace,
          plugin.description,
          plugin.category,
          ...plugin.keywords,
        ]
          .join(" ")
          .toLocaleLowerCase()
          .includes(search)),
  );
}

export function pluginConfigValues(
  fields: Record<string, PluginConfigField>,
  draft: Record<string, unknown>,
): Record<string, unknown> {
  const convert = (field: PluginConfigField, value: unknown): unknown => {
    if (field.type !== "number") return value;
    if (typeof value === "string" && !value.trim())
      throw new Error("Invalid number");
    const number = Number(value);
    if (!Number.isFinite(number)) throw new Error("Invalid number");
    return number;
  };
  return Object.fromEntries(
    Object.entries(draft)
      .filter(([name]) => name in fields)
      .map(([name, value]) => {
        const field = fields[name];
        return [
          name,
          field.multiple && Array.isArray(value)
            ? value.map((entry) => convert(field, entry))
            : convert(field, value),
        ];
      }),
  );
}
