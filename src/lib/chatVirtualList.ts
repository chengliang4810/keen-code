/**
 * 主会话记录使用的可变高度虚拟窗口。
 *
 * 与吸底逻辑协同：
 * - `pinToBottom` 生效时始终挂载最后一行，并从尾部向上构造窗口；
 * - 顶部和底部占位保持总滚动高度稳定，确保吸底与脱离计算有效。
 *
 * 长会话性能策略：
 * - 在累计偏移上使用二分查找，范围定位复杂度为 O(log n)；
 * - 根据视口高度自适应扩展区域；
 * - 用户浏览历史时只就近扩展强制索引，避免挂载远端整段消息。
 */

/** 仅虚拟化长会话；短会话继续挂载完整 DOM。 */
export const CHAT_VIRTUALIZE_THRESHOLD = 48;

/** 单行首次测量前使用的默认高度，单位为像素。 */
export const CHAT_DEFAULT_ROW_ESTIMATE_PX = 120;

/** 单行估算高度上限，避免超长回复主导滚动计算。 */
export const CHAT_MAX_ROW_ESTIMATE_PX = 8000;

/** 浏览历史时视口上下两侧的基础扩展距离，单位为像素。 */
export const CHAT_OVERSCAN_PX = 1200;

/** 吸底时使用的基础扩展距离，保留更多尾部上方内容。 */
export const CHAT_PIN_OVERSCAN_PX = 1600;

/** 普通浏览时自适应扩展距离的下限，单位为像素。 */
export const CHAT_OVERSCAN_MIN_PX = 700;
/** 普通浏览时自适应扩展距离的上限，单位为像素。 */
export const CHAT_OVERSCAN_MAX_PX = 1800;
/** 吸底时自适应扩展距离的下限，单位为像素。 */
export const CHAT_PIN_OVERSCAN_MIN_PX = 1000;
/** 吸底时自适应扩展距离的上限，单位为像素。 */
export const CHAT_PIN_OVERSCAN_MAX_PX = 2400;

/**
 * 已脱离吸底时，为 `forceIndices` 扩展窗口所允许的最大索引间隔。
 * 超出该范围时，查找逻辑需要先把目标粗略滚入自然窗口，避免数百条消息
 * 因远端强制索引而整段挂载。
 */
export const CHAT_FORCE_EXPAND_MAX_GAP = 12;

/**
 * 根据消息内容估算行高，避免图表、表格等高回复首次统一按 120px 估算，
 * 导致总滚动高度过低并误判接近底部。
 *
 * 附件缩略图和内嵌视频不会体现在 `contentLength` 中，因此单独计入，
 * 让首帧估算更接近最终高度并减少底部重测跳动。
 */
export function estimateChatRowHeight(input: {
  /** 消息正文字符数。 */
  contentLength?: number;
  /** 思考内容字符数。 */
  thoughtLength?: number;
  /** 消息角色。 */
  role?: string;
  /** 消息气泡下方的附件卡片数量。 */
  attachmentCount?: number;
  /** 正文是否可能包含本地视频卡片。 */
  hasVideoCard?: boolean;
  /**
   * 是否为已经内联或折叠的工具行。
   * 此类行必须估算为 0，避免长工具链产生虚假占位并显示空白会话。
   */
  collapsed?: boolean;
}): number {
  if (input.collapsed) return 0;
  const content = Math.max(0, input.contentLength ?? 0);
  const thought = Math.max(0, input.thoughtLength ?? 0);
  const role = (input.role ?? "assistant").toLowerCase();
  // 空工具日志行不能增加吸底窗口高度，否则会出现空白会话记录。
  if (
    role === "tool" &&
    content === 0 &&
    thought === 0 &&
    !(input.attachmentCount && input.attachmentCount > 0) &&
    !input.hasVideoCard
  ) {
    return 0;
  }
  // 气泡约每行 42 个字符、行高约 20px，并加上角色外框高度。
  const lines = Math.ceil((content + thought * 0.5) / 42);
  const chrome = role === "user" ? 72 : role === "tool" ? 28 : 96;
  const atts = Math.max(0, input.attachmentCount ?? 0);
  // 缩略图约 64px 加间距，在约 360px 宽度下每行约放 5 个。
  const attRows = atts > 0 ? Math.ceil(atts / 5) : 0;
  const attBoost = attRows * 74;
  const videoBoost = input.hasVideoCard ? 260 : 0;
  const raw = chrome + lines * 20 + attBoost + videoBoost;
  // 工具行较紧凑，不使用 Assistant 默认的 120px 下限。
  const floor = role === "tool" ? 0 : CHAT_DEFAULT_ROW_ESTIMATE_PX;
  return Math.min(CHAT_MAX_ROW_ESTIMATE_PX, Math.max(floor, raw));
}

export type ChatVirtualWindow = {
  /** 挂载窗口的起始索引，包含该索引。 */
  start: number;
  /** 挂载窗口的结束索引，不包含该索引。 */
  end: number;
  /** 已省略顶部内容对应的占位高度。 */
  paddingTop: number;
  /** 已省略底部内容对应的占位高度。 */
  paddingBottom: number;
  /** 全部消息的估算或实测总高度。 */
  totalHeight: number;
};

/** 计算累计偏移：`offsets[i]` 为 `[0, i)` 行高之和，数组长度为 count + 1。 */
export function cumulativeOffsets(
  count: number,
  getHeight: (index: number) => number,
): number[] {
  const offsets = new Array<number>(count + 1);
  offsets[0] = 0;
  for (let i = 0; i < count; i++) {
    const h = Math.max(0, getHeight(i));
    offsets[i + 1] = (offsets[i] ?? 0) + h;
  }
  return offsets;
}

/**
 * 计算自适应扩展距离。
 * 随视口高度缩放，既避免大屏挂载过多 Markdown，也为短视口快速滚动保留缓冲。
 */
export function resolveChatOverscanPx(input: {
  /** 当前滚动视口高度。 */
  viewportHeight: number;
  /** 当前是否保持吸底。 */
  pinToBottom?: boolean;
  /** 测试或调用方提供的显式扩展距离。 */
  overscanPx?: number;
}): number {
  if (input.overscanPx != null && Number.isFinite(input.overscanPx)) {
    return Math.max(0, input.overscanPx);
  }
  const vh = Math.max(0, input.viewportHeight);
  if (input.pinToBottom) {
    // 尾部上方保留约 1.5 个视口，再限制到吸底范围内。
    const raw = vh * 1.5 + 200;
    return Math.round(
      Math.min(
        CHAT_PIN_OVERSCAN_MAX_PX,
        Math.max(CHAT_PIN_OVERSCAN_MIN_PX, raw, CHAT_PIN_OVERSCAN_PX * 0.75),
      ),
    );
  }
  // 浏览历史时保留约一个视口的滚动缓冲。
  const raw = vh * 1.1 + 100;
  return Math.round(
    Math.min(
      CHAT_OVERSCAN_MAX_PX,
      Math.max(CHAT_OVERSCAN_MIN_PX, raw, CHAT_OVERSCAN_PX * 0.6),
    ),
  );
}

/**
 * 查找底边越过 `y` 的第一行，即与 `y` 相交或位于其下方的第一行。
 * `offsets` 长度为 count + 1，复杂度为 O(log n)。
 */
export function findStartIndex(offsets: number[], y: number): number {
  const count = offsets.length - 1;
  if (count <= 0) return 0;
  if (y <= 0) return 0;
  // 查找首个满足 offsets[i + 1] > y 的索引。
  let lo = 0;
  let hi = count - 1;
  let ans = count - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    const bottom = offsets[mid + 1] ?? 0;
    if (bottom > y) {
      ans = mid;
      hi = mid - 1;
    } else {
      lo = mid + 1;
    }
  }
  return ans;
}

/**
 * 查找顶边大于等于 `y` 的第一行，作为不包含的结束候选，复杂度为 O(log n)。
 */
export function findEndIndex(offsets: number[], y: number): number {
  const count = offsets.length - 1;
  if (count <= 0) return 0;
  // 查找首个满足 offsets[i] >= y 的索引；不存在时返回 count。
  let lo = 0;
  let hi = count;
  let ans = count;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (mid >= count) {
      ans = count;
      break;
    }
    const top = offsets[mid] ?? 0;
    if (top >= y) {
      ans = mid;
      hi = mid - 1;
    } else {
      lo = mid + 1;
    }
  }
  return Math.min(count, ans);
}

/**
 * 扩展 `[start, end)` 以包含指定的强制索引。
 * - 吸底时始终扩展，避免工具密集的尾部只挂载空行；
 * - 脱离吸底时仅扩展自然窗口附近的索引，避免浏览历史时挂载整个尾部。
 */
export function applyForceIndices(input: {
  /** 自然窗口起始索引。 */
  start: number;
  /** 自然窗口结束索引。 */
  end: number;
  /** 消息总数。 */
  count: number;
  /** 必须保持挂载的消息索引。 */
  forceIndices?: readonly number[];
  /** 当前是否保持吸底。 */
  pinToBottom?: boolean;
  /** 脱离吸底时允许扩展的最大索引间隔。 */
  maxGap?: number;
}): { start: number; end: number } {
  let { start, end } = input;
  const count = input.count;
  const pin = !!input.pinToBottom;
  const maxGap = input.maxGap ?? CHAT_FORCE_EXPAND_MAX_GAP;
  if (!input.forceIndices?.length || count <= 0) {
    return { start, end };
  }
  for (const raw of input.forceIndices) {
    const i = Math.floor(raw);
    if (i < 0 || i >= count) continue;
    if (pin) {
      if (i < start) start = i;
      if (i >= end) end = i + 1;
      continue;
    }
    // 脱离吸底时只扩展邻近索引。
    if (i < start) {
      if (start - i <= maxGap) start = i;
    }
    if (i >= end) {
      // 计算最后一个已包含索引与目标索引的距离。
      if (i - (end - 1) <= maxGap) end = i + 1;
    }
  }
  return { start, end };
}

/**
 * 计算可变高度列表的可见索引范围及顶部、底部占位。
 */
export function computeChatVirtualWindow(input: {
  /** 消息总数。 */
  count: number;
  /** 获取指定消息的估算或实测高度。 */
  getHeight: (index: number) => number;
  /** 当前滚动位置。 */
  scrollTop: number;
  /** 当前视口高度。 */
  viewportHeight: number;
  /** 可选的显式扩展距离。 */
  overscanPx?: number;
  /** 是否启用吸底；启用时强制包含最后一项。 */
  pinToBottom?: boolean;
  /** 必须保持挂载的索引，例如查找命中项和流式回复。 */
  forceIndices?: readonly number[];
  /**
   * 可选的预计算累计偏移，长度应为 count + 1。
   * 高频滚动调用方可缓存到行高发生变化为止。
   */
  offsets?: readonly number[];
}): ChatVirtualWindow {
  const count = Math.max(0, Math.floor(input.count));
  if (count === 0) {
    return { start: 0, end: 0, paddingTop: 0, paddingBottom: 0, totalHeight: 0 };
  }

  const offsets: number[] =
    input.offsets && input.offsets.length === count + 1
      ? (input.offsets as number[])
      : cumulativeOffsets(count, input.getHeight);
  const totalHeight = offsets[count] ?? 0;
  const viewportHeight = Math.max(0, input.viewportHeight);
  const pin = !!input.pinToBottom;
  const overscan = resolveChatOverscanPx({
    viewportHeight,
    pinToBottom: pin,
    overscanPx: input.overscanPx,
  });

  // 吸底时按视口已停在绝对底部计算，避免 scrollTop 慢一帧导致流式尾部未挂载。
  let viewTop = Math.max(0, input.scrollTop);
  let viewBottom = viewTop + viewportHeight;
  if (pin) {
    viewBottom = totalHeight;
    viewTop = Math.max(0, totalHeight - Math.max(viewportHeight, 1));
  }

  const rangeTop = Math.max(0, viewTop - overscan);
  const rangeBottom = Math.min(totalHeight, viewBottom + overscan);

  let start = findStartIndex(offsets, rangeTop);
  let end = findEndIndex(offsets, rangeBottom);
  if (end <= start) end = Math.min(count, start + 1);

  if (pin) {
    end = count;
  }

  ({ start, end } = applyForceIndices({
    start,
    end,
    count,
    forceIndices: input.forceIndices,
    pinToBottom: pin,
  }));

  start = Math.max(0, Math.min(start, count - 1));
  end = Math.max(start + 1, Math.min(end, count));

  const paddingTop = offsets[start] ?? 0;
  const rendered = (offsets[end] ?? 0) - paddingTop;
  const paddingBottom = Math.max(0, totalHeight - paddingTop - rendered);

  return { start, end, paddingTop, paddingBottom, totalHeight };
}

/**
 * 视口上方的行发生高度变化时修正 scrollTop，避免浏览历史时可见内容跳动。
 *
 * 仅当变化前的整行都位于视口上方时才修正。包含大量媒体的高行可能横跨视口，
 * 若仅凭行顶位于折叠线上方就补偿全部高度差，会把用户拉向底部。
 */
export function scrollTopAfterHeightChange(input: {
  /** 当前滚动位置。 */
  scrollTop: number;
  /** 发生变化的行相对列表顶部的偏移。 */
  rowOffset: number;
  /** 本次重测前已经提交的行高，用于判断是否横跨视口。 */
  prevHeight: number;
  /** 本次行高变化量。 */
  delta: number;
  /** 当前是否保持吸底。 */
  pinToBottom: boolean;
}): number {
  if (input.pinToBottom) return input.scrollTop;
  if (input.delta === 0) return input.scrollTop;
  const oldBottom = input.rowOffset + Math.max(0, input.prevHeight);
  // 旧行整体位于视口上方时补偿高度差，保持阅读锚点稳定。
  if (oldBottom <= input.scrollTop + 0.5) {
    return Math.max(0, input.scrollTop + input.delta);
  }
  // 横跨视口或位于折叠线下方时原地伸缩，不修正滚动位置。
  return input.scrollTop;
}

/**
 * 判断重测结果是否应写入行高缓存。
 * 忽略微小抖动，并过滤 Markdown 或代码重排产生的不稳定小幅收缩。
 */
export function shouldCommitRowHeight(
  prev: number | undefined,
  next: number,
): boolean {
  if (next < 0) return false;
  // 内联工具步骤可能确实为零高；拒绝零高会留下大量虚假滚动空间并显示空白会话。
  if (next === 0) {
    return prev == null || prev !== 0;
  }
  if (prev == null) return true;
  const delta = next - prev;
  if (Math.abs(delta) < 2) return false;
  // 增长直接接受；收缩仅在变化足够明显且稳定时接受。
  if (delta < 0 && Math.abs(delta) < Math.max(24, prev * 0.08)) {
    return false;
  }
  return true;
}
