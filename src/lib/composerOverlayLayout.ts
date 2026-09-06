/** 底部输入区与问答卡片分别占用的垂直空间。 */
export interface ComposerOverlayLayout {
  /** 输入区真实高度；仅用它定位问答卡片，避免使用总留白形成反馈。 */
  composerHeight: number;
  /** 消息列表需要避让的总高度：输入区加问答卡片。 */
  composerFloatPad: number;
}

/** 两个底部区域上下排列；无问答时保持原输入区留白。 */
export function calculateComposerOverlayLayout(
  composerHeight: number,
  questionHeight: number,
): ComposerOverlayLayout {
  return {
    composerHeight: Math.ceil(composerHeight),
    composerFloatPad: Math.ceil(composerHeight + questionHeight),
  };
}
