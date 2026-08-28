/**
 * 会话滚动吸底辅助逻辑。
 *
 * 用户保持跟随时，新内容会让视口继续停在底部。用户主动向上浏览后，不能仅因
 * 视口仍处于“接近底部”范围就重新吸底，否则会在阅读时回弹。只有用户再次向下
 * 到达底部范围、到达绝对底部、发送消息或切换会话时才重新吸底。
 */

/** 重新吸底时仍视为“接近底部”的最大距离，单位为像素。 */
export const STICK_TO_BOTTOM_THRESHOLD_PX = 100;

/**
 * 绝对底部判定带，单位为像素。
 * 用户到达这里时可重新跟随，覆盖已经滚到最大值但最后一次事件没有正增量的情况。
 */
export const STICK_HARD_BOTTOM_PX = 2;

/**
 * 小于该值的字体、思考流和亚像素重排视为噪声，不强制滚动跟随。
 * 阈值略高于 1–2px，避免虚拟列表占位重测持续抖动。
 */
export const STICK_HEIGHT_NOISE_PX = 8;

/**
 * 离开吸底锁定所需的最小向上滚动距离，单位为像素。
 * 用于过滤触控板抖动、弹性滚动和虚拟行高修正产生的微小位移。
 */
export const STICK_ESCAPE_MIN_DELTA_PX = 14;

/**
 * 滚轮向上浏览历史时离开吸底所需的最小 deltaY 绝对值。
 * 过滤底部的轻微触控板滚动，避免先脱离再立刻吸回。
 */
export const STICK_ESCAPE_WHEEL_DELTA = 10;

type ProgrammaticStickScroll = { top: number; at: number };

const PROGRAMMATIC_STICK_SCROLL_TTL_MS = 100;
const programmaticStickScroll = new WeakMap<Element, ProgrammaticStickScroll>();

/** 记录虚拟列表主动写入的吸底位置，避免被滚动监听误判成用户上滑。 */
export function markProgrammaticStickScroll(el: Element, top: number): void {
  programmaticStickScroll.set(el, { top, at: performance.now() });
}

/** 读取并消费仍有效的程序化吸底位置。 */
export function takeProgrammaticStickScroll(el: Element): number | undefined {
  const value = programmaticStickScroll.get(el);
  if (!value) return undefined;
  programmaticStickScroll.delete(el);
  if (performance.now() - value.at > PROGRAMMATIC_STICK_SCROLL_TTL_MS) {
    return undefined;
  }
  return value.top;
}

/** 判断向上滚动是否足以表明用户主动离开底部。 */
export function isMeaningfulScrollUp(
  scrollTop: number,
  previousScrollTop: number,
  minDeltaPx: number = STICK_ESCAPE_MIN_DELTA_PX,
): boolean {
  return previousScrollTop - scrollTop >= minDeltaPx;
}

/**
 * 仅把真正离开底部的向上滚动视为用户脱离；内容收缩导致浏览器钳制到新底部不算。
 */
export function shouldReleaseStickOnScrollUp(input: {
  pinned: boolean;
  scrollTop: number;
  previousScrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}): boolean {
  return (
    input.pinned &&
    isMeaningfulScrollUp(input.scrollTop, input.previousScrollTop) &&
    !isHardBottom(input.scrollTop, input.scrollHeight, input.clientHeight)
  );
}

/** 计算视口当前位置距离内容底部的像素数。 */
export function distanceFromBottom(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
): number {
  return Math.max(0, scrollHeight - clientHeight - scrollTop);
}

/** 判断视口是否足够接近底部，可以重新跟随。 */
export function isNearBottom(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
  thresholdPx: number = STICK_TO_BOTTOM_THRESHOLD_PX,
): boolean {
  // 内容未溢出视口时始终视为位于底部。
  if (scrollHeight <= clientHeight + 1) return true;
  return distanceFromBottom(scrollTop, scrollHeight, clientHeight) <= thresholdPx;
}

/** 判断视口是否停在绝对底部判定带内。 */
export function isHardBottom(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
  hardPx: number = STICK_HARD_BOTTOM_PX,
): boolean {
  if (scrollHeight <= clientHeight + 1) return true;
  return distanceFromBottom(scrollTop, scrollHeight, clientHeight) <= hardPx;
}

/** 计算让视口停在底部所需的 scrollTop。 */
export function bottomScrollTop(
  scrollHeight: number,
  clientHeight: number,
): number {
  return Math.max(0, scrollHeight - clientHeight);
}

/** 判断内容高度变化是否只是无需重新跟随的噪声。 */
export function isHeightDeltaNoise(
  difference: number,
  noisePx: number = STICK_HEIGHT_NOISE_PX,
): boolean {
  return Math.abs(difference) < noisePx;
}

/** 会话滚动 Hook 使用的吸底与脱离锁定状态。 */
export type StickPinState = {
  /** 是否自动跟随内容增长。 */
  pinned: boolean;
  /** 用户是否已主动离开底部；为 true 时阻止仅凭距离阈值重新吸底。 */
  escaped: boolean;
};

/**
 * 根据用户滚动计算吸底状态的纯状态迁移。
 *
 * `userIntentDown` 表示最后一次用户手势朝向最新内容。它与 hardBottom 结合后，
 * 即使最大 scrollTop 处最后一次事件没有正增量，也能重新吸底；但用户向上滚动后
 * 即便仍处于接近底部范围，也不会因此自动吸回。
 */
export function nextStickPinState(
  state: StickPinState,
  input: {
    /** 当前事件是否向上浏览历史。 */
    scrollingUp: boolean;
    /** 当前事件是否向下接近最新内容。 */
    scrollingDown: boolean;
    /** 当前视口是否处于接近底部范围。 */
    nearBottom: boolean;
    /** 当前视口是否停在绝对底部判定带内。 */
    hardBottom?: boolean;
    /** 最后一次用户手势是否朝向底部。 */
    userIntentDown?: boolean;
  },
): StickPinState {
  // 主动向上浏览优先脱离吸底，避免流式增长把用户拉回底部。
  if (input.scrollingUp) {
    return { pinned: false, escaped: true };
  }
  // 用户明确朝向最新内容并到达绝对底部时重新吸底。
  if (input.hardBottom && (input.scrollingDown || input.userIntentDown)) {
    return { pinned: true, escaped: false };
  }
  let { pinned, escaped } = state;
  // 只有真正到达底部范围才解除脱离状态。虚拟列表的估算高度可能暂时偏短，
  // 因此不能在列表中部仅凭一次向下位移就解除。脱离期间还要求明确的向下手势，
  // 防止高度收缩或布局钳制造成的合成滚动重新吸底。
  if (input.scrollingDown && input.nearBottom) {
    if (!escaped || input.userIntentDown) {
      escaped = false;
      pinned = true;
    }
  } else if (!escaped && input.nearBottom) {
    pinned = true;
  }
  return { pinned, escaped };
}
