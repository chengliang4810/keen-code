import type { Tab } from "@/modules/tabs/lib/useTabs";

/** 空项目也要写入空列表，防止最后一个标签关闭后旧存档在重启时复活。 */
export function groupSpaceTabs(
  tabs: Tab[],
  spaceIds: string[],
): Map<string, Tab[]> {
  const groups = new Map<string, Tab[]>(spaceIds.map((id) => [id, []]));
  for (const tab of tabs) {
    const group = groups.get(tab.spaceId);
    if (group) group.push(tab);
    else groups.set(tab.spaceId, [tab]);
  }
  return groups;
}
