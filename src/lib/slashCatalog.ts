/**
 * Slash palette catalog: built-in commands + invocable skills.
 * UI titles/descriptions use i18n keys (`titleKey` / `descriptionKey`)
 * or display strings for dynamic skills.
 */

export type SlashKind = "skill" | "action" | "prompt";

export type SlashItem = {
  id: string;
  kind: SlashKind;
  name: string;
  titleKey?: string;
  descriptionKey?: string;
  displayTitle?: string;
  displayDescription?: string;
  source?: string;
  action?: string;
};

export type SkillInfo = {
  name: string;
  description: string;
  source?: string;
  userInvocable?: boolean;
};

/** 首版保留的桌面端 Slash 命令。 */
export function builtinSlashItems(): SlashItem[] {
  return [
    {
      id: "goal",
      kind: "action",
      name: "goal",
      titleKey: "slash.goal",
      descriptionKey: "slash.goalDesc",
      action: "goal",
    },
    {
      id: "plan",
      kind: "action",
      name: "plan",
      titleKey: "slash.plan",
      descriptionKey: "slash.planDesc",
      action: "plan",
    },
  ];
}

/** Map skill metadata to slash items (skips `userInvocable: false`). */
export function skillsToSlashItems(skills: SkillInfo[]): SlashItem[] {
  // Dedupe by name — duplicate ids (`skill:foo`) break React keys and leave
  // ghost rows that ignore filter updates (always visible, not keyboard-navable).
  const seen = new Set<string>();
  const out: SlashItem[] = [];
  for (const s of skills) {
    if (s.userInvocable === false) continue;
    const name = (s.name ?? "").trim();
    if (!name || seen.has(name)) continue;
    seen.add(name);
    out.push({
      id: `skill:${name}`,
      kind: "skill" as const,
      name,
      displayTitle: name,
      displayDescription: s.description,
      source: s.source,
    });
  }
  return out;
}

/** Optional resolved UI strings (i18n titles / descriptions) for search. */
export type SlashSearchText = {
  title?: string;
  description?: string;
};

/**
 * Filter items by query (case-insensitive substring).
 * Prefer name/title hits; descriptions only when query is longer (4+ chars)
 * so short tokens don't light up half the catalog via English blurbs.
 * Empty query returns all items.
 */
export function filterSlashItems(
  items: SlashItem[],
  query: string,
  resolveSearchText?: (item: SlashItem) => SlashSearchText | null | undefined,
): SlashItem[] {
  const q = query.trim().toLowerCase();
  if (!q) return items;
  return items.filter((item) => {
    const resolved = resolveSearchText?.(item);
    // Name / title only for short queries (strict).
    const nameFields = [
      item.name,
      item.displayTitle,
      // strip "skill:" prefix from id for matching
      item.id?.replace(/^skill:/, ""),
      resolved?.title,
    ];
    if (nameFields.some((f) => f && f.toLowerCase().includes(q))) return true;
    // Description: ASCII needs 4+ chars (avoid "the"/"and" style noise);
    // CJK tokens are already dense at 2 characters.
    const asciiOnly = /^[\x00-\x7f]+$/.test(q);
    if (q.length < (asciiOnly ? 4 : 2)) return false;
    const descFields = [item.displayDescription, resolved?.description];
    return descFields.some((f) => f && f.toLowerCase().includes(q));
  });
}

/** Full catalog split into built-in commands and skill items. */
export function buildSlashCatalog(skills: SkillInfo[]): {
  commands: SlashItem[];
  skills: SlashItem[];
} {
  return {
    commands: builtinSlashItems(),
    skills: skillsToSlashItems(skills),
  };
}

/** Flat list for keyboard nav: filtered commands then skills. */
export function flattenFilteredCatalog(
  catalog: { commands: SlashItem[]; skills: SlashItem[] },
  query: string,
  resolveSearchText?: (item: SlashItem) => SlashSearchText | null | undefined,
): { commands: SlashItem[]; skills: SlashItem[]; flat: SlashItem[] } {
  const commands = filterSlashItems(catalog.commands, query, resolveSearchText);
  const skills = filterSlashItems(catalog.skills, query, resolveSearchText);
  return { commands, skills, flat: [...commands, ...skills] };
}
