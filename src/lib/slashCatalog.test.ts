import { describe, expect, it } from "vitest";
import {
  buildSlashCatalog,
  builtinSlashItems,
  filterSlashItems,
  skillsToSlashItems,
  type SkillInfo,
  type SlashItem,
} from "./slashCatalog";

describe("builtinSlashItems", () => {
  it("保留会话级 Goal 与计划模式命令", () => {
    const items = builtinSlashItems();
    const names = items.map((i) => i.name);
    expect(names).toEqual(["goal", "plan"]);

    const goal = items.find((i) => i.name === "goal")!;
    expect(goal.kind).toBe("action");
    expect(goal.action).toBe("goal");

    const plan = items.find((i) => i.name === "plan")!;
    expect(plan.kind).toBe("action");
    expect(plan.action).toBe("plan");
    expect(plan.titleKey).toBe("slash.plan");
    expect(plan.descriptionKey).toBe("slash.planDesc");

    expect(items.map((item) => item.action)).not.toEqual(
      expect.arrayContaining(["status", "healthcheck", "newChat", "export"]),
    );
    expect(items).toHaveLength(2);
  });
});

describe("skillsToSlashItems", () => {
  it("maps skill info to slash items", () => {
    const skills: SkillInfo[] = [
      {
        name: "aihot",
        description: "Hot tips",
        source: "user",
        userInvocable: true,
      },
      { name: "hidden", description: "nope", userInvocable: false },
    ];
    const items = skillsToSlashItems(skills);
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      id: "skill:aihot",
      kind: "skill",
      name: "aihot",
      displayTitle: "aihot",
      displayDescription: "Hot tips",
      source: "user",
    });
  });

  it("includes skills when userInvocable is undefined", () => {
    expect(
      skillsToSlashItems([{ name: "x", description: "d" }]),
    ).toHaveLength(1);
  });
});

describe("filterSlashItems", () => {
  const items: SlashItem[] = [
    {
      id: "status",
      kind: "action",
      name: "status",
      titleKey: "slash.status",
      action: "status",
    },
    {
      id: "skill:aihot",
      kind: "skill",
      name: "aihot",
      displayTitle: "aihot",
      displayDescription: "AI hot reload helper",
    },
    {
      id: "runner",
      kind: "action",
      name: "runner",
      displayDescription: "process diagnostics",
    },
  ];

  it("returns all on empty query", () => {
    expect(filterSlashItems(items, "")).toHaveLength(3);
    expect(filterSlashItems(items, "  ")).toHaveLength(3);
  });

  it("filters by name substring", () => {
    expect(filterSlashItems(items, "sta").map((i) => i.name)).toEqual([
      "status",
    ]);
    expect(filterSlashItems(items, "aih").map((i) => i.name)).toEqual([
      "aihot",
    ]);
  });

  it("filters by description only when query length >= 4", () => {
    expect(filterSlashItems(items, "process").map((i) => i.name)).toEqual([
      "runner",
    ]);
    // "hot" is 3 chars — name-only; aihot matches by name, runner does not
    expect(filterSlashItems(items, "hot").map((i) => i.name)).toEqual([
      "aihot",
    ]);
  });

  it("does not match description for short queries", () => {
    const onlyName = filterSlashItems(items, "p").map((i) => i.name);
    expect(onlyName).not.toContain("runner");
  });

  it("dedupes skills by name", () => {
    const skills: SkillInfo[] = [
      { name: "make-pdf", description: "a" },
      { name: "make-pdf", description: "b" },
      { name: "docx", description: "c" },
    ];
    const items = skillsToSlashItems(skills);
    expect(items.map((i) => i.name)).toEqual(["make-pdf", "docx"]);
  });

  it("is case-insensitive", () => {
    expect(filterSlashItems(items, "STATUS").map((i) => i.name)).toEqual([
      "status",
    ]);
  });

  it("matches resolved Chinese i18n titles", () => {
    const resolve = (item: SlashItem) => {
      if (item.name === "status") return { title: "状态", description: "查看状态" };
      if (item.name === "aihot")
        return { title: "aihot", description: "中文资讯热点" };
      return {};
    };
    expect(
      filterSlashItems(items, "状态", resolve).map((i) => i.name),
    ).toEqual(["status"]);
    expect(
      filterSlashItems(items, "资讯", resolve).map((i) => i.name),
    ).toEqual(["aihot"]);
  });

  it("matches Chinese in displayDescription without resolver", () => {
    const zh: SlashItem[] = [
      {
        id: "skill:x",
        kind: "skill",
        name: "x",
        displayTitle: "x",
        displayDescription: "查询 AI 热点新闻",
      },
    ];
    expect(filterSlashItems(zh, "热点").map((i) => i.name)).toEqual(["x"]);
  });
});

describe("buildSlashCatalog", () => {
  it("保留 Goal/Plan 命令和可调用 Skills", () => {
    const skills: SkillInfo[] = [
      { name: "s1", description: "one" },
      { name: "s2", description: "two", userInvocable: false },
    ];
    const cat = buildSlashCatalog(skills);
    expect(cat.commands).toEqual(builtinSlashItems());
    expect(cat.commands.map((item) => item.name)).toEqual(["goal", "plan"]);
    expect(cat.skills).toHaveLength(1);
    expect(cat.skills[0]!.name).toBe("s1");
  });
});
