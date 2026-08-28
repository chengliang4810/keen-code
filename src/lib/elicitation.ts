import type { AskUserPayload } from "@/lib/session";

function isObjectRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

export function readElicitationRpcId(value: unknown): number | null {
  if (!isObjectRecord(value)) return null;
  return typeof value.rpcId === "number" && Number.isSafeInteger(value.rpcId)
    ? value.rpcId
    : null;
}

/** 将当前 ACP form elicitation 契约严格转换为工作台问答载荷。 */
export function parseElicitationPayload(value: unknown): AskUserPayload | null {
  if (!isObjectRecord(value) || value.method !== "elicitation/create") return null;
  const rpcId = readElicitationRpcId(value);
  const params = value.params;
  if (rpcId == null || !isObjectRecord(params) || params.mode !== "form") return null;
  const sessionId = typeof params.sessionId === "string" ? params.sessionId.trim() : "";
  const schema = params.requestedSchema;
  if (!sessionId || !isObjectRecord(schema) || schema.type !== "object") return null;
  const properties = schema.properties;
  if (!isObjectRecord(properties)) return null;

  const questions: AskUserPayload["questions"] = [];
  for (const [id, rawProperty] of Object.entries(properties)) {
    if (!id || !isObjectRecord(rawProperty)) return null;
    const multiSelect = rawProperty.type === "array";
    if (!multiSelect && rawProperty.type !== "string") return null;

    let rawOptions: unknown;
    if (multiSelect) {
      if (!isObjectRecord(rawProperty.items)) return null;
      rawOptions = rawProperty.items.anyOf;
    } else {
      rawOptions = rawProperty.oneOf;
    }
    if (rawOptions !== undefined && !Array.isArray(rawOptions)) return null;
    if (multiSelect && !Array.isArray(rawOptions)) return null;

    const options = (rawOptions ?? []).map((rawOption) => {
      if (!isObjectRecord(rawOption) || typeof rawOption.const !== "string") return null;
      const optionId = rawOption.const;
      const label =
        typeof rawOption.title === "string" && rawOption.title.trim()
          ? rawOption.title.trim()
          : optionId;
      const description =
        typeof rawOption.description === "string" && rawOption.description.trim()
          ? rawOption.description.trim()
          : undefined;
      return { id: optionId, label, ...(description ? { description } : {}) };
    });
    if (options.some((option) => option == null)) return null;

    const question =
      (typeof rawProperty.description === "string" ? rawProperty.description.trim() : "") ||
      (typeof rawProperty.title === "string" ? rawProperty.title.trim() : "") ||
      (typeof params.message === "string" ? params.message.trim() : "") ||
      id;
    questions.push({
      id,
      question,
      options: options as AskUserPayload["questions"][number]["options"],
      ...(multiSelect ? { multiSelect: true } : {}),
    });
  }
  return questions.length === 0 ? null : { rpcId, sessionId, questions };
}

/** 按问题标识把问答弹窗结果转换为 ACP schema 字段与选项值。 */
export function toElicitationAnswers(
  payload: AskUserPayload,
  modalAnswers: Record<string, string>,
): Record<string, string | string[]> {
  const answers: Record<string, string | string[]> = {};
  for (const question of payload.questions) {
    const rawAnswer = modalAnswers[question.id];
    if (rawAnswer == null) continue;
    const optionValue = (label: string) =>
      question.options.find((option) => option.label === label)?.id ?? label;
    answers[question.id] = question.multiSelect
      ? rawAnswer.split(", ").map(optionValue)
      : optionValue(rawAnswer);
  }
  return answers;
}
