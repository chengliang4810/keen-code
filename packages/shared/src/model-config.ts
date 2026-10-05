import { z } from "zod";
import { compileModelOptionMap } from "@zcode/model-option-map";
import { sparseShape } from "./config-schema.js";

function optionMapSchema(variableName: "reasoningLevel" | "maxOutputTokens") {
  return z
    .string()
    .min(1)
    .superRefine((source, context) => {
      try {
        compileModelOptionMap(source, variableName);
      } catch (error) {
        context.addIssue({
          code: "custom",
          message: error instanceof Error ? error.message : "Option map 无法编译",
        });
      }
    });
}

const enumOptionValueSchema = z
  .string()
  .refine((value) => value.trim().length > 0, "reasoningLevel.values 必须是非空字符串");

function enumOptionValuesSchema(requireValue: boolean) {
  const values = z.array(enumOptionValueSchema);
  return (requireValue ? values.min(1, "reasoningLevel.values 不能为空") : values)
    .refine((items) => new Set(items).size === items.length, "reasoningLevel.values 不能重复")
    .readonly();
}

export const completeEnumOptionSpecDataSchema = z
  .object({
    /** 按语义强度从低到高排列；首项是辅助调用可选的最低公开档位。 */
    values: enumOptionValuesSchema(true),
    map: optionMapSchema("reasoningLevel"),
  })
  .strict();

export const completeLimitOptionSpecDataSchema = z
  .object({
    max: z.number().int().positive(),
    map: optionMapSchema("maxOutputTokens"),
  })
  .strict();

/**
 * Personal Overlay 可以显式保存空档位，表示用户关闭该能力；完整目录配置仍
 * 通过 completeEnumOptionSpecDataSchema 要求至少一个值，避免把空目录发布给 Runtime。
 */
export const enumOptionSpecDataSchema = z
  .object({
    values: enumOptionValuesSchema(false).nullable().optional(),
    map: optionMapSchema("reasoningLevel").nullable().optional(),
  })
  .strict();
export const limitOptionSpecDataSchema = z
  .object(sparseShape(completeLimitOptionSpecDataSchema.shape))
  .strict();

export const completeModelInputFormatDataSchema = z
  .object({
    supportsText: z.boolean(),
    supportsImage: z.boolean(),
    supportsVideo: z.boolean(),
    supportsAudio: z.boolean(),
    supportsPdf: z.boolean(),
  })
  .strict();
export const completeModelOutputFormatDataSchema = z.object({ supportsText: z.boolean() }).strict();
export const modelInputFormatDataSchema = z
  .object(sparseShape(completeModelInputFormatDataSchema.shape))
  .strict();
export const modelOutputFormatDataSchema = z
  .object(sparseShape(completeModelOutputFormatDataSchema.shape))
  .strict();

export const completeModelPropertiesDataSchema = z
  .object({
    requiresMfjsToolSchema: z.boolean(),
    contextWindow: z.number().int().positive(),
    inputFormat: completeModelInputFormatDataSchema,
    outputFormat: completeModelOutputFormatDataSchema,
    supportsToolCall: z.boolean(),
    supportsJsonSchemaOutput: z.boolean(),
    supportsNativeWebSearch: z.boolean(),
    supportsMidConversationSystem: z.boolean(),
  })
  .strict();
export const modelPropertiesDataSchema = z
  .object({
    ...sparseShape(completeModelPropertiesDataSchema.shape),
    inputFormat: modelInputFormatDataSchema.nullable().optional(),
    outputFormat: modelOutputFormatDataSchema.nullable().optional(),
  })
  .strict();

export const completeModelOptionSpecsDataSchema = z
  .object({
    // 并非每个模型都提供推理档位；字段缺席表示模型按默认请求参数执行。
    // 一旦提供，仍必须经过完整 enum schema，拒绝空值、重复值和无效映射。
    reasoningLevel: completeEnumOptionSpecDataSchema.optional(),
    maxOutputTokens: completeLimitOptionSpecDataSchema,
  })
  .strict();
export const modelOptionSpecsDataSchema = z
  .object({
    ...sparseShape(completeModelOptionSpecsDataSchema.shape),
    reasoningLevel: enumOptionSpecDataSchema.nullable().optional(),
    maxOutputTokens: limitOptionSpecDataSchema.nullable().optional(),
  })
  .strict();

export const completeModelConfigDataSchema = z
  .object({
    enabled: z.boolean(),
    properties: completeModelPropertiesDataSchema,
    optionSpecs: completeModelOptionSpecsDataSchema,
  })
  .strict();
export const modelConfigDataSchema = z
  .object({
    ...sparseShape(completeModelConfigDataSchema.shape),
    properties: modelPropertiesDataSchema.nullable().optional(),
    optionSpecs: modelOptionSpecsDataSchema.nullable().optional(),
  })
  .strict();

// 跨层只共享数据合同；Provider 行为类与 IO 不进入公共 Schema。
export type ModelInputFormatData = z.infer<typeof completeModelInputFormatDataSchema>;
export type ModelOutputFormatData = z.infer<typeof completeModelOutputFormatDataSchema>;
export type ModelPropertiesData = z.infer<typeof completeModelPropertiesDataSchema>;
export type EnumOptionSpecData = z.infer<typeof completeEnumOptionSpecDataSchema>;
export type LimitOptionSpecData = z.infer<typeof completeLimitOptionSpecDataSchema>;
export type ModelOptionSpecsData = z.infer<typeof completeModelOptionSpecsDataSchema>;
