import { z } from "zod";

import { STORAGE_CATEGORY_IDS } from "./storage.js";

const nonNegativeNumber = z.number().finite().nonnegative();
const nonNegativeInteger = z.number().int().nonnegative();

/** ResourceManager 原生 DTO 的 strict wire schema。null CPU 表示没有采样基线。 */
export const resourceUsageProcessSchema = z
  .object({
    pid: nonNegativeInteger,
    name: z.string(),
    category: z.enum(["base", "builtin-plugin", "community-plugin"]),
    groupKey: z.string(),
    groupLabel: z.string(),
    cpuPercent: nonNegativeNumber.nullable(),
    memoryBytes: nonNegativeInteger,
    sampled: z.boolean(),
  })
  .strict();

export const resourceUsageSnapshotSchema = z
  .object({
    sampledAt: nonNegativeInteger,
    logicalCpuCount: z.number().int().positive(),
    system: z
      .object({
        cpuPercent: nonNegativeNumber.nullable(),
        memoryTotalBytes: nonNegativeInteger,
        memoryUsedBytes: nonNegativeInteger,
      })
      .strict(),
    app: z
      .object({
        cpuPercent: nonNegativeNumber.nullable(),
        memoryBytes: nonNegativeInteger,
      })
      .strict(),
    processes: z.array(resourceUsageProcessSchema),
  })
  .strict();

export const storageEntryUsageSchema = z
  .object({
    relativePath: z.string(),
    bytes: nonNegativeInteger,
    fileCount: nonNegativeInteger,
  })
  .strict();

export const storageCategoryUsageSchema = z
  .object({
    id: z.enum(STORAGE_CATEGORY_IDS),
    bytes: nonNegativeInteger,
    fileCount: nonNegativeInteger,
    cleanability: z.enum(["none", "safe", "confirm"]),
    entries: z.array(storageEntryUsageSchema),
  })
  .strict();

export const storageVolumeSchema = z
  .object({
    deviceId: z.string(),
    mountPoint: z.string(),
    totalBytes: nonNegativeInteger,
    freeBytes: nonNegativeInteger,
  })
  .strict();

export const storageRootUsageSchema = z
  .object({
    id: z.enum(["home", "dataBaseDir"]),
    path: z.string(),
    volume: storageVolumeSchema.nullable(),
    bytes: nonNegativeInteger,
    fileCount: nonNegativeInteger,
    categories: z.array(storageCategoryUsageSchema),
  })
  .strict();

export const storagePathErrorSchema = z
  .object({
    path: z.string(),
    code: z.string(),
  })
  .strict();

export const storageUsageSnapshotSchema = z
  .object({
    jobId: z.string().min(1),
    status: z.enum(["scanning", "complete", "cancelled", "failed"]),
    startedAt: nonNegativeInteger,
    finishedAt: nonNegativeInteger.optional(),
    roots: z.array(storageRootUsageSchema),
    errors: z.array(storagePathErrorSchema),
  })
  .strict();
