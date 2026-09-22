import type { AskUserPayload } from "@/lib/session";
import type { AcpElicitationClientRequest } from "@/lib/acp/events";

/** 标准 ACP `_meta` 中承载 KeenCode 交互扩展的唯一命名空间。 */
const KEENCODE_META_KEY = "_keencode";

function isObjectRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

/** 将当前 ACP form elicitation 契约严格转换为工作台问答载荷。 */
export function parseElicitationPayload(
  value: AcpElicitationClientRequest,
): AskUserPayload | null {
  const rpcId = value.id;
  const params = value.params;
  if (!isObjectRecord(params) || params.mode !== "form") return null;
  const sessionId = typeof params.sessionId === "string" ? params.sessionId.trim() : "";
  const schema = params.requestedSchema;
  if (!sessionId || !isObjectRecord(schema) || schema.type !== "object") return null;
  const properties = schema.properties;
  if (!isObjectRecord(properties)) return null;
  const meta = params._meta;
  if (meta !== undefined && !isObjectRecord(meta)) return null;
  const keenCodeMeta = meta?.[KEENCODE_META_KEY];
  if (keenCodeMeta !== undefined && !isObjectRecord(keenCodeMeta)) return null;
  const askUserMeta = keenCodeMeta?.askUser;
  if (askUserMeta !== undefined && !isObjectRecord(askUserMeta)) return null;
  const allowCustomByQuestion = askUserMeta?.allowCustomByQuestion;
  if (
    allowCustomByQuestion !== undefined &&
    !isObjectRecord(allowCustomByQuestion)
  ) return null;

  // 标准 Schema 的属性映射不承诺提问顺序；自有问答必须显式给出完整且唯一的顺序。
  // 不带自有问答扩展的标准 ACP 表单按自身 Schema 解析，不推测任何旧问答字段。
  const propertyIds = Object.keys(properties);
  const questionOrder = askUserMeta === undefined
    ? propertyIds
    : askUserMeta.questionOrder;
  if (
    !Array.isArray(questionOrder) ||
    questionOrder.length !== propertyIds.length ||
    questionOrder.some((id) =>
      typeof id !== "string" || !id ||
      !Object.prototype.hasOwnProperty.call(properties, id),
    ) ||
    new Set(questionOrder).size !== questionOrder.length
  ) return null;

  const questions: AskUserPayload["questions"] = [];
  for (const id of questionOrder as string[]) {
    const rawProperty = properties[id];
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
    const explicitAllowCustom = allowCustomByQuestion?.[id];
    if (
      explicitAllowCustom !== undefined &&
      typeof explicitAllowCustom !== "boolean"
    ) return null;

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
      allowCustomAnswer: explicitAllowCustom ?? rawOptions === undefined,
    });
  }
  return questions.length === 0
    ? null
    : {
        rpcId,
        sessionId,
        ...(params.toolCallId ? { toolCallId: params.toolCallId } : {}),
        questions,
      };
}

/** 按问题标识把问答弹窗结果转换为 ACP schema 字段与选项值。 */
export function toElicitationAnswers(
  payload: AskUserPayload,
  modalAnswers: Record<string, string | string[]>,
): Record<string, string | string[]> {
  const answers: Record<string, string | string[]> = {};
  for (const question of payload.questions) {
    const rawAnswer = modalAnswers[question.id];
    if (rawAnswer == null) continue;
    const optionValue = (value: string) =>
      question.options.find((option) => option.id === value || option.label === value)?.id ?? value;
    if (question.multiSelect) {
      if (!Array.isArray(rawAnswer)) {
        throw new Error("多选问答必须使用字符串数组提交");
      }
      answers[question.id] = rawAnswer.map(optionValue);
    } else {
      if (Array.isArray(rawAnswer)) {
        throw new Error("单选问答必须使用字符串提交");
      }
      answers[question.id] = optionValue(rawAnswer);
    }
  }
  return answers;
}
