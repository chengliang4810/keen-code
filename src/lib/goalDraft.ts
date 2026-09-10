/** 与 ACP Goal 字段的 UTF-8 字节限制一致；标题只作摘要，正文完整保存。 */
const MAX_GOAL_TITLE_BYTES = 512;
const MAX_GOAL_OBJECTIVE_BYTES = 64 * 1024;

/** 仅承载本地校验类别，界面按当前语言呈现，不走远端错误脱敏兜底。 */
export class GoalDraftValidationError extends Error {
  constructor(readonly reason: "empty" | "tooLarge") {
    super(reason === "empty" ? "目标正文不能为空" : "目标正文不能超过 65536 个 UTF-8 字节");
  }
}

/** 在创建乐观消息前校验目标，并按完整 Unicode 字符生成有界标题。 */
export function buildGoalDraft(text: string) {
  const objective = text.trim();
  const encoder = new TextEncoder();
  if (!objective) throw new GoalDraftValidationError("empty");
  if (encoder.encode(objective).length > MAX_GOAL_OBJECTIVE_BYTES) {
    throw new GoalDraftValidationError("tooLarge");
  }
  const firstLine = objective.split(/\r?\n/, 1)[0];
  let title = firstLine;
  if (encoder.encode(title).length > MAX_GOAL_TITLE_BYTES) {
    title = "";
    let bytes = encoder.encode("…").length;
    for (const character of firstLine) {
      bytes += encoder.encode(character).length;
      if (bytes > MAX_GOAL_TITLE_BYTES) break;
      title += character;
    }
    title += "…";
  }
  return { title, objective };
}
