const KNOB_SIZE = 36;

export function effortStopPosition(
  index: number,
  count: number,
  width: number,
) {
  const inset = Math.min(KNOB_SIZE, width) / 2;
  const progress = count > 1 ? Math.max(0, index) / (count - 1) : 0;
  return inset + Math.min(1, progress) * Math.max(0, width - inset * 2);
}

export function effortPointerPosition(
  clientX: number,
  left: number,
  renderedWidth: number,
  layoutWidth: number,
) {
  const inset = Math.min(KNOB_SIZE, layoutWidth) / 2;
  const x =
    renderedWidth > 0
      ? ((clientX - left) / renderedWidth) * layoutWidth
      : inset;
  return Math.max(inset, Math.min(layoutWidth - inset, x));
}
