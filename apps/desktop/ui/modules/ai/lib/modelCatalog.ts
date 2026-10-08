export type CatalogModel = {
  id: string;
  canonicalId?: string | null;
  contextLimit: number;
  outputLimit?: number | null;
  vision?: boolean | null;
  reasoning?: boolean | null;
  reasoningLevels?: string[] | null;
};

export type CatalogProvider = {
  id: string;
  api?: string | null;
  models: CatalogModel[];
};

export type ModelCatalogSnapshot = {
  schemaVersion: number;
  fetchedAt: number;
  providers: CatalogProvider[];
};

type Candidate = { provider: CatalogProvider; model: CatalogModel };
export type ModelCatalogIndex = {
  aliases: ReadonlyMap<string, readonly Candidate[]>;
  sites: ReadonlyMap<string, readonly { id: string; path: string }[]>;
};

const DEFAULT_APIS: Readonly<Record<string, string>> = {
  openai: "https://api.openai.com/v1",
  anthropic: "https://api.anthropic.com/v1",
  google: "https://generativelanguage.googleapis.com/v1beta",
  xai: "https://api.x.ai/v1",
  groq: "https://api.groq.com/openai/v1",
  cerebras: "https://api.cerebras.ai/v1",
  mistral: "https://api.mistral.ai/v1",
};

function key(id: string): string {
  return id.trim().toLowerCase();
}

function tail(id: string): string {
  return id.slice(id.lastIndexOf("/") + 1);
}

export function createModelCatalogIndex(
  snapshot: ModelCatalogSnapshot,
): ModelCatalogIndex {
  if (
    snapshot.schemaVersion !== 1 ||
    !Number.isSafeInteger(snapshot.fetchedAt) ||
    !Array.isArray(snapshot.providers) ||
    !snapshot.providers.length
  )
    throw new Error("Invalid model catalog.");
  const aliases = new Map<string, Candidate[]>();
  const sites = new Map<string, { id: string; path: string }[]>();
  for (const provider of snapshot.providers) {
    if (!provider.id || !Array.isArray(provider.models))
      throw new Error("Invalid model catalog provider.");
    const api = provider.api ?? DEFAULT_APIS[provider.id];
    const target = api ? url(api) : undefined;
    if (target) {
      const entries = sites.get(target.origin) ?? [];
      entries.push({
        id: provider.id,
        path: target.pathname.replace(/\/+$/, ""),
      });
      sites.set(target.origin, entries);
    }
    for (const model of provider.models) {
      if (
        !model.id ||
        !Number.isSafeInteger(model.contextLimit) ||
        model.contextLimit < 8192 ||
        model.contextLimit > 4_000_000 ||
        (model.outputLimit != null &&
          (!Number.isSafeInteger(model.outputLimit) || model.outputLimit <= 0))
      )
        throw new Error("Invalid model catalog limits.");
      const candidate = { provider, model };
      const names = new Set([
        key(model.id),
        key(tail(model.id)),
        ...(model.canonicalId
          ? [key(model.canonicalId), key(tail(model.canonicalId))]
          : []),
      ]);
      for (const name of names) {
        const entries = aliases.get(name) ?? [];
        entries.push(candidate);
        aliases.set(name, entries);
      }
    }
  }
  return { aliases, sites };
}

function url(value: string): URL | undefined {
  try {
    const parsed = new URL(value);
    if (
      !["https:", "http:"].includes(parsed.protocol) ||
      parsed.username ||
      parsed.password
    )
      return;
    return parsed;
  } catch {
    return;
  }
}

function providerMatch(apiPath: string, basePath: string): number {
  return basePath === apiPath ||
    basePath.startsWith(`${apiPath}/`) ||
    apiPath === `${basePath}/v1`
    ? apiPath.length
    : -1;
}

function unanimous(candidates: readonly Candidate[]): CatalogModel | undefined {
  const first = candidates[0]?.model;
  if (
    first &&
    candidates.every(
      ({ model }) =>
        model.contextLimit === first.contextLimit &&
        model.outputLimit === first.outputLimit &&
        model.vision === first.vision &&
        model.reasoning === first.reasoning &&
        JSON.stringify(model.reasoningLevels) ===
          JSON.stringify(first.reasoningLevels),
    )
  )
    return first;
}

export function resolveCatalogModel(
  index: ModelCatalogIndex | null,
  id: string,
  baseURL?: string,
): CatalogModel | undefined {
  if (!index || !id.trim()) return;
  const identity = key(id);
  const candidates =
    index.aliases.get(identity) ?? index.aliases.get(tail(identity)) ?? [];
  const base = baseURL ? url(baseURL.trim()) : undefined;
  if (base) {
    const basePath = base.pathname.replace(/\/+$/, "");
    const matches = (index.sites.get(base.origin) ?? []).map((provider) => ({
      id: provider.id,
      score: providerMatch(provider.path, basePath),
    }));
    const best = Math.max(-1, ...matches.map(({ score }) => score));
    if (best >= 0) {
      const providers = new Set(
        matches.filter(({ score }) => score === best).map(({ id }) => id),
      );
      return unanimous(
        candidates.filter(({ provider }) => providers.has(provider.id)),
      );
    }
  }
  const owners = candidates.filter(({ provider, model }) => {
    const canonical = model.canonicalId ?? model.id;
    return key(canonical.split("/")[0]) === key(provider.id);
  });
  return unanimous(owners.length ? owners : candidates);
}

let catalog: ModelCatalogIndex | null = null;

export function installModelCatalog(
  snapshot: ModelCatalogSnapshot | null,
): void {
  catalog = snapshot ? createModelCatalogIndex(snapshot) : null;
}

export function catalogModelRecommendation(id: string, baseURL?: string) {
  return resolveCatalogModel(catalog, id, baseURL);
}
