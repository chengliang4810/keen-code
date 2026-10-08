export type EffortTrackKind = "plain" | "supercharged" | "fast" | "fusion";
export interface EffortTrackPalette {
  brand: string;
  fast: string;
  spark: string;
}
export interface EffortTrackSize {
  width: number;
  height: number;
}
export interface EffortTrackFrame {
  kind: EffortTrackKind;
  width: number;
  height: number;
  time: number;
  palette: EffortTrackPalette;
}
export const EFFORT_TRACK_FALLBACK_PALETTE: EffortTrackPalette = {
  brand: "#8c57f7",
  fast: "#d9b8ff",
  spark: "#ffffff",
};
const TAU = Math.PI * 2;
const EDGE_FADE = 14;
const BOLT_FLASH = 0.26;
const SHEEN_PERIOD = 2.8;
export function resolveEffortTrackKind(
  isMax: boolean,
  fast: boolean,
): EffortTrackKind {
  if (isMax && fast) return "fusion";
  if (isMax) return "supercharged";
  if (fast) return "fast";
  return "plain";
}
export function resolveFixedEffortTrackKind(isMax: boolean): EffortTrackKind {
  return resolveEffortTrackKind(isMax, true);
}
export function trackFrameIntervalMs(kind: EffortTrackKind): number {
  return kind === "supercharged" ? 1000 / 30 : 1000 / 60;
}
export function noise(index: number, trait: number): number {
  const value = Math.sin(index * 12.9898 + trait * 78.233) * 43758.5453;
  return value - Math.floor(value);
}
export function edgeFade(x: number, width: number): number {
  return Math.min(
    1,
    Math.max(0, x / EDGE_FADE),
    Math.max(0, (width - x) / EDGE_FADE),
  );
}
export interface EffortRgb {
  r: number;
  g: number;
  b: number;
}
const HEX_COLOR = /^#([0-9a-f]{3,8})$/i;
const RGB_COLOR = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)/i;
export function parseColor(value: string): EffortRgb | null {
  const text = value.trim();
  const hex = HEX_COLOR.exec(text);
  if (hex) {
    const digits = hex[1];
    if (digits.length === 3 || digits.length === 4) {
      return {
        r: parseInt(digits.charAt(0) + digits.charAt(0), 16),
        g: parseInt(digits.charAt(1) + digits.charAt(1), 16),
        b: parseInt(digits.charAt(2) + digits.charAt(2), 16),
      };
    }
    if (digits.length === 6 || digits.length === 8) {
      return {
        r: parseInt(digits.slice(0, 2), 16),
        g: parseInt(digits.slice(2, 4), 16),
        b: parseInt(digits.slice(4, 6), 16),
      };
    }
    return null;
  }
  const rgb = RGB_COLOR.exec(text);
  if (!rgb) return null;
  return { r: Number(rgb[1]), g: Number(rgb[2]), b: Number(rgb[3]) };
}
export function withAlpha(color: EffortRgb, alpha: number): string {
  const clamp = (n: number) => Math.max(0, Math.min(255, Math.round(n)));
  return `rgba(${clamp(color.r)}, ${clamp(color.g)}, ${clamp(color.b)}, ${alpha})`;
}
export function readEffortTrackPalette(
  styles: Pick<CSSStyleDeclaration, "getPropertyValue">,
): EffortTrackPalette {
  const read = (name: string, fallback: string) => {
    const value = styles.getPropertyValue(name).trim();
    return value || fallback;
  };
  return {
    brand: read("--effort-brand-solid", EFFORT_TRACK_FALLBACK_PALETTE.brand),
    fast: read("--effort-fast", EFFORT_TRACK_FALLBACK_PALETTE.fast),
    spark: read("--effort-spark", EFFORT_TRACK_FALLBACK_PALETTE.spark),
  };
}
export function readEffortPaletteFromElement(
  element: Element | null,
): EffortTrackPalette {
  if (typeof window === "undefined" || !element) {
    return EFFORT_TRACK_FALLBACK_PALETTE;
  }
  return readEffortTrackPalette(window.getComputedStyle(element));
}
export type TrackCanvas = Pick<
  CanvasRenderingContext2D,
  | "fillStyle"
  | "strokeStyle"
  | "lineWidth"
  | "lineCap"
  | "lineJoin"
  | "globalCompositeOperation"
  | "shadowBlur"
  | "shadowColor"
  | "beginPath"
  | "moveTo"
  | "lineTo"
  | "closePath"
  | "arc"
  | "rect"
  | "fill"
  | "stroke"
  | "save"
  | "restore"
  | "clip"
  | "createLinearGradient"
  | "createRadialGradient"
>;
export function drawEffortTrackFrame(
  ctx: TrackCanvas,
  frame: EffortTrackFrame,
): void {
  const { kind, width, height, time, palette } = frame;
  if (width <= 8) return;
  const size = { width, height };
  if (kind === "fusion") {
    drawFusion(ctx, size, time, palette);
  }
  if (kind === "supercharged" || kind === "fusion") {
    drawParticles(ctx, size, time, palette.spark);
  }
  if (kind === "fast" || kind === "fusion") {
    drawStreaks(ctx, size, time, palette.spark);
    drawBolts(ctx, size, time, palette.fast, palette.spark);
  }
}
function drawParticles(
  ctx: TrackCanvas,
  size: EffortTrackSize,
  time: number,
  color: string,
): void {
  const rgb = parseColor(color);
  if (!rgb) return;
  const span = size.width + 8;
  const count = Math.max(10, Math.floor(size.width / 5));
  for (let index = 0; index < count; index += 1) {
    const seed = index;
    const height = noise(seed, 1);
    const speed = 12 + 28 * noise(seed, 2);
    const radius = 0.7 + 1.1 * noise(seed, 3);
    const start = noise(seed, 4) * span;
    const brightness = 0.22 + 0.5 * noise(seed, 5);
    const x = ((start + time * speed) % span) - 4;
    const y = size.height * (0.16 + 0.68 * height);
    const shimmer =
      0.6 + 0.4 * Math.sin(time * (2 + 3 * noise(seed, 6)) + seed);
    const opacity = brightness * shimmer * edgeFade(x, size.width);
    if (opacity <= 0.01) continue;
    ctx.fillStyle = withAlpha(rgb, opacity);
    ctx.beginPath();
    ctx.arc(x, y, radius, 0, TAU);
    ctx.fill();
  }
}
function drawStreaks(
  ctx: TrackCanvas,
  size: EffortTrackSize,
  time: number,
  spark: string,
): void {
  const head = parseColor(spark);
  if (!head) return;
  const width = size.width;
  const count = Math.max(5, Math.floor(width / 14));
  for (let index = 0; index < count; index += 1) {
    const seed = index + 100;
    const length = 10 + 20 * noise(seed, 1);
    const speed = 110 + 190 * noise(seed, 2);
    const thickness = 1.1 + 1.1 * noise(seed, 3);
    const span = width + length + 12;
    const x = ((noise(seed, 4) * span + time * speed) % span) - length - 6;
    const y = size.height * (0.2 + 0.6 * noise(seed, 5));
    const brightness =
      (0.35 + 0.55 * noise(seed, 6)) * edgeFade(x + length, width);
    if (brightness <= 0.02) continue;
    const gradient = ctx.createLinearGradient(x, y, x + length, y);
    gradient.addColorStop(0, withAlpha(head, 0));
    gradient.addColorStop(1, withAlpha(head, brightness));
    ctx.fillStyle = gradient;
    roundedRectPath(
      ctx,
      x,
      y - thickness / 2,
      length,
      thickness,
      thickness / 2,
    );
    ctx.fill();
  }
}
function drawBolts(
  ctx: TrackCanvas,
  size: EffortTrackSize,
  time: number,
  color: string,
  core: string,
): void {
  const rgb = parseColor(color);
  const coreRgb = parseColor(core);
  if (!rgb || !coreRgb) return;
  const width = size.width;
  const height = size.height;
  for (let slot = 0; slot < 2; slot += 1) {
    const seed = slot + 200;
    const period = 1.4 + 0.9 * noise(seed, 1);
    const offset = noise(seed, 2) * period;
    const cycle = Math.floor((time + offset) / period);
    const phase = time + offset - cycle * period;
    if (phase >= BOLT_FLASH) continue;
    const intensity = Math.sin((phase / BOLT_FLASH) * Math.PI);
    const cycleSeed = seed + cycle * 7.31;
    const x = 12 + (width - 24) * noise(cycleSeed, 3);
    if (x <= 6 || x >= width - 6) continue;
    const lean = 3 + 3 * noise(cycleSeed, 4);
    ctx.save();
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.shadowColor = withAlpha(rgb, 0.9 * intensity);
    ctx.shadowBlur = 6;
    ctx.strokeStyle = withAlpha(rgb, 0.9 * intensity);
    ctx.lineWidth = 4;
    boltPath(ctx, x, lean, height);
    ctx.stroke();
    ctx.shadowBlur = 0;
    ctx.strokeStyle = withAlpha(coreRgb, intensity);
    ctx.lineWidth = 1.5;
    ctx.stroke();
    ctx.restore();
  }
}
function boltPath(
  ctx: TrackCanvas,
  x: number,
  lean: number,
  height: number,
): void {
  ctx.beginPath();
  ctx.moveTo(x + lean, 3);
  ctx.lineTo(x - lean * 0.4, height * 0.42);
  ctx.lineTo(x + lean * 0.5, height * 0.5);
  ctx.lineTo(x - lean, height - 3);
}
function drawSheen(
  ctx: TrackCanvas,
  size: EffortTrackSize,
  time: number,
  color: string,
): void {
  const rgb = parseColor(color);
  if (!rgb) return;
  const width = size.width;
  const height = size.height;
  const band = 46;
  const progress = (time / SHEEN_PERIOD) % 1;
  const x = -band + (width + band * 2) * progress;
  const gradient = ctx.createLinearGradient(x, 0, x + band + 10, 0);
  gradient.addColorStop(0, withAlpha(rgb, 0));
  gradient.addColorStop(0.5, withAlpha(rgb, 0.22));
  gradient.addColorStop(1, withAlpha(rgb, 0));
  ctx.fillStyle = gradient;
  ctx.beginPath();
  ctx.moveTo(x + 10, 0);
  ctx.lineTo(x + band + 10, 0);
  ctx.lineTo(x + band, height);
  ctx.lineTo(x, height);
  ctx.closePath();
  ctx.fill();
}
function drawFusion(
  ctx: TrackCanvas,
  size: EffortTrackSize,
  time: number,
  palette: EffortTrackPalette,
): void {
  const lead = parseColor(palette.brand);
  const head = parseColor(palette.fast);
  if (!lead || !head) return;
  const width = size.width;
  const height = size.height;
  const seam =
    width *
    (0.5 + 0.13 * Math.sin(time * 0.7) + 0.05 * Math.sin(time * 1.9 + 1));
  const pulse = 0.5 + 0.5 * Math.sin(time * 2.4);
  const gradient = ctx.createLinearGradient(0, height / 2, width, height / 2);
  const seamAt = seam / width;
  gradient.addColorStop(0, withAlpha(lead, 1));
  gradient.addColorStop(Math.max(0, seamAt - 0.22), withAlpha(lead, 1));
  gradient.addColorStop(seamAt, withAlpha({ r: 255, g: 255, b: 255 }, 0.92));
  gradient.addColorStop(Math.min(1, seamAt + 0.16), withAlpha(head, 1));
  gradient.addColorStop(1, withAlpha(head, 1));
  ctx.fillStyle = gradient;
  ctx.beginPath();
  ctx.moveTo(0, 0);
  ctx.lineTo(width, 0);
  ctx.lineTo(width, height);
  ctx.lineTo(0, height);
  ctx.closePath();
  ctx.fill();
  drawPlasma(ctx, size, time, seam, lead, head);
  drawSeamSparks(ctx, size, time, seam, lead, head);
  drawSeamCore(ctx, size, seam, pulse);
  drawSheen(ctx, size, time, palette.spark);
}
function drawPlasma(
  ctx: TrackCanvas,
  size: EffortTrackSize,
  time: number,
  seam: number,
  lead: EffortRgb,
  head: EffortRgb,
): void {
  const width = size.width;
  const height = size.height;
  const reach = width * 0.55;
  for (let band = 0; band < 3; band += 1) {
    const seed = band + 300;
    const amplitude = height * (0.12 + 0.16 * noise(seed, 1));
    const wavelength = 30 + 34 * noise(seed, 2);
    const speed = 22 + 34 * noise(seed, 3);
    const thickness = 1.8 + 2.2 * noise(seed, 4);
    const phase = ((time * speed) / wavelength) * TAU;
    for (let stream = 0; stream < 2; stream += 1) {
      const toNear = stream === 0;
      const streamPhase = phase + seed * (toNear ? 1 : 1.7);
      const steps = Math.floor(width / 3) + 1;
      ctx.save();
      ctx.beginPath();
      ctx.moveTo(toNear ? 0 : seam, 0);
      ctx.lineTo(toNear ? seam : width, 0);
      ctx.lineTo(toNear ? seam : width, height);
      ctx.lineTo(toNear ? 0 : seam, height);
      ctx.closePath();
      ctx.clip();
      const color = toNear ? head : lead;
      const brightness = 0.32 + 0.16 * noise(seed, 5);
      const gradient = ctx.createLinearGradient(
        toNear ? seam - reach : seam,
        0,
        toNear ? seam : seam + reach,
        0,
      );
      gradient.addColorStop(0, withAlpha(color, toNear ? 0 : brightness));
      gradient.addColorStop(1, withAlpha(color, toNear ? brightness : 0));
      ctx.globalCompositeOperation = "lighter";
      ctx.shadowColor = withAlpha(color, brightness);
      ctx.shadowBlur = 5;
      ctx.lineCap = "round";
      ctx.lineJoin = "round";
      ctx.strokeStyle = gradient;
      ctx.lineWidth = thickness;
      ctx.beginPath();
      for (let step = 0; step <= steps; step += 1) {
        const x = Math.min(width, step * 3);
        const y =
          height / 2 +
          amplitude *
            Math.sin(
              (x / wavelength) * TAU + (toNear ? streamPhase : -streamPhase),
            );
        if (step === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      ctx.stroke();
      ctx.restore();
    }
  }
}
function drawSeamSparks(
  ctx: TrackCanvas,
  size: EffortTrackSize,
  time: number,
  seam: number,
  lead: EffortRgb,
  head: EffortRgb,
): void {
  const width = size.width;
  const height = size.height;
  for (let index = 0; index < 22; index += 1) {
    const seed = index + 400;
    const life = 0.7 + 0.8 * noise(seed, 1);
    const age = (time + noise(seed, 2) * life) % life;
    const progress = age / life;
    const toNear = noise(seed, 3) < 0.5;
    const speed = 28 + 74 * noise(seed, 4);
    const x = seam + (toNear ? -1 : 1) * speed * age;
    const y =
      height * (0.18 + 0.64 * noise(seed, 5)) + 5 * Math.sin(age * 8 + seed);
    const radius = 0.7 + 1.5 * (1 - progress);
    const opacity =
      (1 - progress) * (0.5 + 0.5 * noise(seed, 6)) * edgeFade(x, width);
    if (opacity <= 0.01) continue;
    const landed = toNear ? lead : head;
    ctx.fillStyle = withAlpha(
      {
        r: 255 + (landed.r - 255) * progress * 0.8,
        g: 255 + (landed.g - 255) * progress * 0.8,
        b: 255 + (landed.b - 255) * progress * 0.8,
      },
      opacity,
    );
    ctx.beginPath();
    ctx.arc(x, y, radius, 0, TAU);
    ctx.fill();
  }
}
function drawSeamCore(
  ctx: TrackCanvas,
  size: EffortTrackSize,
  seam: number,
  pulse: number,
): void {
  const height = size.height;
  const white = { r: 255, g: 255, b: 255 };
  const coreWidth = 9 + 8 * pulse;
  const glow = ctx.createRadialGradient(
    seam,
    height / 2,
    0,
    seam,
    height / 2,
    coreWidth,
  );
  glow.addColorStop(0, withAlpha(white, 0.32 + 0.3 * pulse));
  glow.addColorStop(1, withAlpha(white, 0));
  ctx.save();
  ctx.globalCompositeOperation = "lighter";
  ctx.fillStyle = glow;
  ctx.beginPath();
  ctx.moveTo(seam - coreWidth, 0);
  ctx.lineTo(seam + coreWidth, 0);
  ctx.lineTo(seam + coreWidth, height);
  ctx.lineTo(seam - coreWidth, height);
  ctx.closePath();
  ctx.fill();
  ctx.restore();
  ctx.fillStyle = withAlpha(white, 0.5 + 0.4 * pulse);
  roundedRectPath(ctx, seam - 1.1, 2, 2.2, height - 4, 1.1);
  ctx.fill();
}
function roundedRectPath(
  ctx: TrackCanvas,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
): void {
  const r = Math.max(0, Math.min(radius, width / 2, height / 2));
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.lineTo(x + width - r, y);
  ctx.arc(x + width - r, y + r, r, -Math.PI / 2, 0);
  ctx.lineTo(x + width, y + height - r);
  ctx.arc(x + width - r, y + height - r, r, 0, Math.PI / 2);
  ctx.lineTo(x + r, y + height);
  ctx.arc(x + r, y + height - r, r, Math.PI / 2, Math.PI);
  ctx.lineTo(x, y + r);
  ctx.arc(x + r, y + r, r, Math.PI, Math.PI * 1.5);
  ctx.closePath();
}
