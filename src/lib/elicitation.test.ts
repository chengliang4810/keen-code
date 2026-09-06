import { describe, expect, it } from "vitest";
import type { AcpElicitationClientRequest } from "./acp/events";
import { parseElicitationPayload, toElicitationAnswers } from "./elicitation";

/** 模拟标准 Rust Schema 字典序序列化后的三个问题，另带原始问题顺序。 */
function orderedRequest(
  questionOrder: unknown = ["target", "checks", "note"],
): AcpElicitationClientRequest {
  return {
    jsonrpc: "2.0",
    id: "ask-order",
    method: "elicitation/create",
    params: {
      mode: "form",
      sessionId: "session-order",
      toolCallId: "call-order",
      message: "补充验收选择",
      requestedSchema: {
        type: "object",
        properties: {
          checks: {
            type: "array",
            description: "选择检查项目",
            items: { anyOf: [{ const: "格式,严格" }, { const: "测试" }] },
          },
          note: { type: "string", description: "补充说明" },
          target: {
            type: "string",
            description: "选择运行位置",
            oneOf: [{ const: "本机" }, { const: "远端" }],
          },
        },
      },
      _meta: {
        _keencode: {
          askUser: {
            questionOrder,
            allowCustomByQuestion: { target: false, checks: false, note: true },
          },
        },
      },
    },
  };
}

describe("AskUser 显式问题顺序", () => {
  it("JSON 往返后仍按模型输入顺序显示，不按 Schema 字典序排列", () => {
    const request = JSON.parse(JSON.stringify(orderedRequest()));
    expect(Object.keys(request.params.requestedSchema.properties)).toEqual([
      "checks", "note", "target",
    ]);
    const payload = parseElicitationPayload(request);
    expect(payload?.questions.map((question) => question.id)).toEqual([
      "target", "checks", "note",
    ]);
    expect(payload?.questions.map((question) => question.allowCustomAnswer)).toEqual([
      false, false, true,
    ]);
    expect(payload?.toolCallId).toBe("call-order");
  });

  it("数字形式的问题标识也不能被 JavaScript 属性枚举重新排序", () => {
    const request = orderedRequest(["10", "2", "1"]);
    request.params.requestedSchema.properties = {
      "1": { type: "string" }, "2": { type: "string" }, "10": { type: "string" },
    };
    expect(parseElicitationPayload(request)?.questions.map((question) => question.id))
      .toEqual(["10", "2", "1"]);
  });

  it.each([
    ["缺失", undefined],
    ["空数组", []],
    ["非数组", "target,checks,note"],
    ["缺少问题", ["target", "checks"]],
    ["未知问题", ["target", "checks", "missing"]],
    ["重复问题", ["target", "checks", "checks"]],
    ["非字符串", ["target", "checks", 1]],
    ["原型键", ["target", "checks", "toString"]],
    ["额外问题", ["target", "checks", "note", "extra"]],
  ])("拒绝 KeenCode 问答顺序%s，不静默回退到字典序", (_name, order) => {
    const request = orderedRequest(order);
    // undefined 必须真实代表缺失，而不是触发 helper 的默认参数。
    if (order === undefined) {
      request.params._meta = { _keencode: { askUser: {} } };
    }
    expect(parseElicitationPayload(request)).toBeNull();
  });

  it("无 KeenCode 问答扩展的标准 ACP 表单继续按 Schema 解析", () => {
    const request = orderedRequest();
    delete request.params._meta;
    expect(parseElicitationPayload(request)?.questions.map((question) => question.id))
      .toEqual(["checks", "note", "target"]);
  });

  it("重排只影响展示，不改变答案标识、数组或中文正文", () => {
    const payload = parseElicitationPayload(orderedRequest());
    expect(payload).not.toBeNull();
    expect(toElicitationAnswers(payload!, {
      checks: ["格式,严格", "测试"], note: "中文,不拼接", target: "本机",
    })).toEqual({ target: "本机", checks: ["格式,严格", "测试"], note: "中文,不拼接" });
  });
});
