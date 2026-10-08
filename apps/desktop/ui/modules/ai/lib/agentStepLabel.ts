import { t } from "@/modules/i18n/state";

/** 执行状态保留原文，只翻译显示前缀，命令、路径和智能体名称保持原样。 */
export function agentStepLabel(step: string, localize: typeof t = t): string {
  const subagent = /^Spawning (.+) subagent$/.exec(step);
  if (subagent)
    return localize("Spawning {agent} subagent", { agent: subagent[1] });
  const plan = /^Updating plan \((\d+) items\)$/.exec(step);
  if (plan)
    return localize("Updating plan ({count} items)", { count: plan[1] });
  for (const verb of [
    "Reading",
    "Listing",
    "Grepping",
    "Globbing",
    "Editing",
    "Writing",
    "Creating",
    "Running",
    "Spawning",
    "Suggesting",
  ]) {
    const prefix = `${verb} `;
    if (step.startsWith(prefix))
      return localize(`${verb} {value}`, { value: step.slice(prefix.length) });
  }
  return localize(step);
}
