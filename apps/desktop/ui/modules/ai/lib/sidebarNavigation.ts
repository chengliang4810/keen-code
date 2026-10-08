import type { SessionMeta } from "@/modules/ai/lib/sessions";
import type { SpaceMeta } from "@/modules/spaces/lib/store";

export type SidebarPin = { kind: "project" | "task"; id: string };
export type SidebarSectionId = "pinned" | "projects" | "conversations";
export type ConversationSort = "recent" | "manual";
export type SidebarNavigationSnapshot = {
  pins: SidebarPin[];
  collapsed: SidebarSectionId[];
  /** 仅覆盖左侧项目栏目顺序；缺省时沿用项目存档的原始顺序。 */
  projectOrder?: string[];
  /** 缺省为最近更新，兼容已有导航存档。 */
  conversationSort?: ConversationSort;
  /** 手动顺序独立保存，切回最近更新时保留，置顶项暂时隐藏也不丢位置。 */
  conversationOrder?: string[];
};

export function normalizeSidebarNavigation(
  value: unknown,
): SidebarNavigationSnapshot {
  const record =
    value && typeof value === "object"
      ? (value as Record<string, unknown>)
      : {};
  const pins: SidebarPin[] = [];
  const seen = new Set<string>();
  if (Array.isArray(record.pins)) {
    for (const pin of record.pins) {
      if (
        !pin ||
        (pin.kind !== "project" && pin.kind !== "task") ||
        typeof pin.id !== "string" ||
        !pin.id.trim()
      )
        continue;
      const key = `${pin.kind}:${pin.id}`;
      if (seen.has(key)) continue;
      seen.add(key);
      pins.push({ kind: pin.kind, id: pin.id });
    }
  }
  const sections: SidebarSectionId[] = ["pinned", "projects", "conversations"];
  const projectOrder = Array.isArray(record.projectOrder)
    ? [
        ...new Set(
          record.projectOrder.filter(
            (id): id is string => typeof id === "string" && Boolean(id.trim()),
          ),
        ),
      ]
    : [];
  const conversationOrder = Array.isArray(record.conversationOrder)
    ? [
        ...new Set(
          record.conversationOrder.filter(
            (id): id is string => typeof id === "string" && Boolean(id.trim()),
          ),
        ),
      ]
    : [];
  return {
    pins,
    ...(projectOrder.length ? { projectOrder } : {}),
    ...(record.conversationSort === "manual" ||
    record.conversationSort === "recent"
      ? { conversationSort: record.conversationSort }
      : {}),
    ...(conversationOrder.length ? { conversationOrder } : {}),
    collapsed: sections.filter(
      (id) => Array.isArray(record.collapsed) && record.collapsed.includes(id),
    ),
  };
}

export type ProjectDropTarget = SidebarPin & {
  section: SidebarSectionId;
  owner?: string;
};
export type ProjectDropEdge = "before" | "after";

export function orderedSidebarProjectIds(
  ids: readonly string[],
  order: readonly string[] = [],
): string[] {
  const available = new Set(ids);
  return [...new Set([...order.filter((id) => available.has(id)), ...ids])];
}

/** 新建对话显示在最前，已有对话继续保留手动顺序，避免新任务藏在“展开显示”后。 */
function orderedConversationIds(
  ids: readonly string[],
  order: readonly string[] = [],
): string[] {
  const available = new Set(ids);
  const known = new Set(order);
  return [
    ...new Set([
      ...ids.filter((id) => !known.has(id)),
      ...order.filter((id) => available.has(id)),
    ]),
  ];
}

/** 只在原栏目中移动项目，拖拽不会改变置顶状态、归属或当前对话。 */
export function moveSidebarProject(
  snapshot: SidebarNavigationSnapshot,
  projectIds: readonly string[],
  source: ProjectDropTarget,
  target: ProjectDropTarget,
  edge: ProjectDropEdge,
): SidebarNavigationSnapshot {
  if (
    source.kind !== "project" ||
    source.section === "conversations" ||
    source.section !== target.section ||
    !projectIds.includes(source.id)
  )
    return snapshot;
  const key = (item: SidebarPin) => `${item.kind}:${item.id}`;
  const items: SidebarPin[] =
    source.section === "pinned"
      ? snapshot.pins
      : orderedSidebarProjectIds(projectIds, snapshot.projectOrder)
          .filter(
            (id) =>
              !snapshot.pins.some(
                (pin) => pin.kind === "project" && pin.id === id,
              ),
          )
          .map((id) => ({ kind: "project", id }));
  if (
    key(source) === key(target) ||
    !items.some((item) => key(item) === key(source)) ||
    !items.some((item) => key(item) === key(target))
  )
    return snapshot;
  const reordered = items.filter((item) => key(item) !== key(source));
  const index = reordered.findIndex((item) => key(item) === key(target));
  reordered.splice(index + (edge === "after" ? 1 : 0), 0, {
    kind: source.kind,
    id: source.id,
  });
  if (reordered.every((item, i) => key(item) === key(items[i])))
    return snapshot;
  if (source.section === "pinned") return { ...snapshot, pins: reordered };
  // 被置顶的项目保留其普通栏目位置，取消置顶后仍回到该顺序。
  let position = 0;
  const visible = new Set(items.map((item) => item.id));
  const projectOrder = orderedSidebarProjectIds(
    projectIds,
    snapshot.projectOrder,
  ).map((id) => (visible.has(id) ? reordered[position++].id : id));
  return { ...snapshot, projectOrder };
}

/** 首次切换手动时冻结当前顺序，之后切换模式不覆盖已有手动排序。 */
export function setSidebarConversationSort(
  snapshot: SidebarNavigationSnapshot,
  sort: ConversationSort,
  sessions: readonly SessionMeta[],
): SidebarNavigationSnapshot {
  if ((snapshot.conversationSort ?? "recent") === sort) return snapshot;
  return {
    ...snapshot,
    conversationSort: sort,
    ...(sort === "manual" && !snapshot.conversationOrder
      ? {
          conversationOrder: [...sessions]
            .sort((a, b) => b.updatedAt - a.updatedAt)
            .map((task) => task.id),
        }
      : {}),
  };
}

export function conversationOwner(task: SessionMeta): string {
  return task.projectless ? "projectless" : `project:${task.projectId ?? ""}`;
}

/** 拖拽只调整同一归属及同一归档视图内的对话，不移动会话或修改项目绑定。 */
export function moveSidebarConversation(
  snapshot: SidebarNavigationSnapshot,
  sessions: readonly SessionMeta[],
  sourceId: string,
  targetId: string,
  edge: ProjectDropEdge,
): SidebarNavigationSnapshot {
  if (snapshot.conversationSort !== "manual" || sourceId === targetId)
    return snapshot;
  const source = sessions.find((task) => task.id === sourceId);
  const target = sessions.find((task) => task.id === targetId);
  if (
    !source ||
    !target ||
    conversationOwner(source) !== conversationOwner(target) ||
    Boolean(source.archived) !== Boolean(target.archived)
  )
    return snapshot;
  const pinned = new Set(
    snapshot.pins.filter((pin) => pin.kind === "task").map((pin) => pin.id),
  );
  if (pinned.has(sourceId) || pinned.has(targetId)) return snapshot;
  const order = orderedConversationIds(
    [...sessions]
      .sort((a, b) => b.createdAt - a.createdAt || a.id.localeCompare(b.id))
      .map((task) => task.id),
    snapshot.conversationOrder,
  );
  const eligible = new Set(
    sessions
      .filter(
        (task) =>
          conversationOwner(task) === conversationOwner(source) &&
          Boolean(task.archived) === Boolean(source.archived) &&
          !pinned.has(task.id),
      )
      .map((task) => task.id),
  );
  const items = order.filter((id) => eligible.has(id));
  const moved = items.filter((id) => id !== sourceId);
  moved.splice(
    moved.indexOf(targetId) + (edge === "after" ? 1 : 0),
    0,
    sourceId,
  );
  if (moved.every((id, index) => id === items[index])) return snapshot;
  let index = 0;
  return {
    ...snapshot,
    conversationOrder: order.map((id) =>
      eligible.has(id) ? moved[index++] : id,
    ),
  };
}

export function toggleSidebarPin(
  snapshot: SidebarNavigationSnapshot,
  pin: SidebarPin,
): SidebarNavigationSnapshot {
  const exists = snapshot.pins.some(
    (item) => item.kind === pin.kind && item.id === pin.id,
  );
  return {
    ...snapshot,
    pins: exists
      ? snapshot.pins.filter(
          (item) => item.kind !== pin.kind || item.id !== pin.id,
        )
      : [...snapshot.pins, pin],
  };
}

export function toggleSidebarSection(
  snapshot: SidebarNavigationSnapshot,
  id: SidebarSectionId,
): SidebarNavigationSnapshot {
  return {
    ...snapshot,
    collapsed: snapshot.collapsed.includes(id)
      ? snapshot.collapsed.filter((item) => item !== id)
      : [...snapshot.collapsed, id],
  };
}

export type SidebarProjectGroup = { project: SpaceMeta; tasks: SessionMeta[] };
export type PinnedSidebarItem =
  | { kind: "project"; group: SidebarProjectGroup }
  | { kind: "task"; task: SessionMeta };

/** 置顶仅改变展示位置，父子同时置顶时对话只出现一次，归属保持不变。 */
export function sidebarNavigationGroups(
  projects: readonly SpaceMeta[],
  sessions: readonly SessionMeta[],
  pins: readonly SidebarPin[],
  archived: boolean,
  projectOrder: readonly string[] = [],
  conversationSort: ConversationSort = "recent",
  conversationOrder: readonly string[] = [],
) {
  const removedProjects = new Set(
    projects.filter((project) => project.removed).map((project) => project.id),
  );
  projects = projects.filter((project) => !project.removed);
  const pinnedProjects = new Set(
    pins.filter((p) => p.kind === "project").map((p) => p.id),
  );
  const pinnedTasks = new Set(
    pins.filter((p) => p.kind === "task").map((p) => p.id),
  );
  const visibleTasks = sessions
    .filter(
      (task) =>
        Boolean(task.archived) === archived &&
        (task.projectless ||
          !task.projectId ||
          !removedProjects.has(task.projectId)),
    )
    .sort((a, b) => b.updatedAt - a.updatedAt);
  if (conversationSort === "manual") {
    const ranks = new Map(
      orderedConversationIds(
        [...visibleTasks]
          .sort((a, b) => b.createdAt - a.createdAt || a.id.localeCompare(b.id))
          .map((task) => task.id),
        conversationOrder,
      ).map((id, index) => [id, index]),
    );
    visibleTasks.sort(
      (a, b) => (ranks.get(a.id) ?? 0) - (ranks.get(b.id) ?? 0),
    );
  }
  const projectsById = new Map(
    projects.map((project) => [project.id, project]),
  );
  const groups = orderedSidebarProjectIds(
    projects.map((project) => project.id),
    projectOrder,
  ).flatMap((id) => {
    const project = projectsById.get(id);
    return project
      ? [
          {
            project,
            tasks: visibleTasks.filter(
              (task) =>
                !task.projectless &&
                task.projectId === id &&
                !pinnedTasks.has(task.id),
            ),
          },
        ]
      : [];
  });
  const byProject = new Map(groups.map((group) => [group.project.id, group]));
  const byTask = new Map(visibleTasks.map((task) => [task.id, task]));
  const pinned: PinnedSidebarItem[] = [];
  const seen = new Set<string>();
  for (const pin of pins) {
    const key = `${pin.kind}:${pin.id}`;
    if (seen.has(key)) continue;
    seen.add(key);
    if (pin.kind === "project") {
      const group = byProject.get(pin.id);
      if (group) pinned.push({ kind: "project", group });
    } else {
      const task = byTask.get(pin.id);
      if (task) pinned.push({ kind: "task", task });
    }
  }
  return {
    pinned,
    projects: groups.filter((group) => !pinnedProjects.has(group.project.id)),
    conversations: visibleTasks.filter(
      (task) => task.projectless && !pinnedTasks.has(task.id),
    ),
  };
}
