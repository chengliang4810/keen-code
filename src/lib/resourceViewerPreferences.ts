import type { OpenLocationTarget } from "@/components/OpenLocationButton";

/** 资源树宽度的当前持久化键。 */
export const RESOURCE_TREE_WIDTH_STORAGE_KEY = "keencode.resource-tree-width";
/** 系统打开目标的当前持久化键。 */
export const RESOURCE_OPEN_TARGET_STORAGE_KEY = "keencode.open-target";
/** 资源树首次启动宽度。 */
export const RESOURCE_TREE_WIDTH_DEFAULT = 220;
/** 资源树允许的最小宽度。 */
export const RESOURCE_TREE_WIDTH_MIN = 140;
/** 资源树允许的最大宽度。 */
export const RESOURCE_TREE_WIDTH_MAX = 420;

/** 读取资源树宽度；只有键缺失时使用首次启动值。 */
export function loadResourceTreeWidth(
  storage: Pick<Storage, "getItem"> = localStorage,
): number {
  const raw = storage.getItem(RESOURCE_TREE_WIDTH_STORAGE_KEY);
  if (raw === null) return RESOURCE_TREE_WIDTH_DEFAULT;
  if (!/^(?:0|[1-9]\d*)$/.test(raw)) {
    throw new Error("资源树宽度格式无效");
  }
  const width = Number(raw);
  if (
    !Number.isSafeInteger(width) ||
    width < RESOURCE_TREE_WIDTH_MIN ||
    width > RESOURCE_TREE_WIDTH_MAX
  ) {
    throw new Error("资源树宽度超出范围");
  }
  return width;
}

/** 校验并写入资源树宽度。 */
export function saveResourceTreeWidth(
  width: number,
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  if (
    !Number.isSafeInteger(width) ||
    width < RESOURCE_TREE_WIDTH_MIN ||
    width > RESOURCE_TREE_WIDTH_MAX
  ) {
    throw new Error("资源树宽度超出范围");
  }
  storage.setItem(RESOURCE_TREE_WIDTH_STORAGE_KEY, String(width));
}

/** 读取当前支持的系统打开目标。 */
export function loadResourceOpenTarget(
  storage: Pick<Storage, "getItem"> = localStorage,
): OpenLocationTarget {
  const value = storage.getItem(RESOURCE_OPEN_TARGET_STORAGE_KEY);
  if (value === null) return "finder";
  if (value === "finder" || value === "explorer" || value === "system") {
    return value;
  }
  throw new Error(`无效的打开目标：${value}`);
}

/** 校验并写入系统打开目标。 */
export function saveResourceOpenTarget(
  target: OpenLocationTarget,
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  if (target !== "finder" && target !== "explorer" && target !== "system") {
    throw new Error(`无效的打开目标：${String(target)}`);
  }
  storage.setItem(RESOURCE_OPEN_TARGET_STORAGE_KEY, target);
}
