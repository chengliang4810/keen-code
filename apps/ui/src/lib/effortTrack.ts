/**
 * 思考强度轨道的四态外观与 Canvas 动效规则。
 *
 * 对照 Droppy Code 的 `EffortSlider`（Swift/SwiftUI）移植：四态由「是否最高档」
 * 与「是否快速」两个布尔量组合而来；所有动效都是时间的纯函数，逐帧重算而不
 * 保存粒子状态，因此同一时刻的画面可复现。
 *
 * 与上游的三处有意差异：
 * 1. 不使用 `CanvasRenderingContext2D.filter`（Safari 18 才支持，本项目的
 *    WebView 目标更旧），辉光改用 `shadowBlur` 与径向渐变。
 * 2. 上游在 fusion 态把扫光画了两遍（`drawFusion` 内一次、`body` 内又一次），
 *    此处只画一遍。
 * 3. 上游按供应商切换品牌色并支持「亮填充反色」（银白品牌）；本项目的供应商
 *    ID 是用户自定义字符串，无法可靠映射，因此品牌色固定为陶土橙，亮填充
 *    分支随之取消。
 */

/** 轨道的四种外观；`plain` 不绘制动效。 */
export type EffortTrackKind = "plain" | "supercharged" | "fast" | "fusion";

/** 画布动效用到的颜色。DOM 侧的填充与标题色由 CSS 令牌负责。 */
export interface EffortTrackPalette {
  /** 品牌色，最高档的填充与融合渐变近端。 */
  brand: string;
  /** 快速色，闪电辉光与融合渐变远端。 */
  fast: string;
  /** 粒子与扫光的颜色。 */
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
  /** 自本次动效开始经过的秒数。 */
  time: number;
  palette: EffortTrackPalette;
}

/** 令牌缺失时的兜底色，与 `tokens.css` 中的取值一致。 */
export const EFFORT_TRACK_FALLBACK_PALETTE: EffortTrackPalette = {
  brand: "#cc6b47",
  fast: "#ffd65c",
  spark: "#ffffff",
};

const TAU = Math.PI * 2;
/** 粒子与色带在填充两端各自淡出的宽度（点）。 */
const EDGE_FADE = 14;
/** 闪电每次闪烁的持续时间（秒）。 */
const BOLT_FLASH = 0.26;
/** 扫光扫过一遍的周期（秒）。 */
const SHEEN_PERIOD = 2.8;

/**
 * 由最高档与快速两个布尔量决定轨道外观。
 * 四态互斥，`plain` 是唯一不挂载画布的状态。
 */
export function resolveEffortTrackKind(
  isMax: boolean,
  fast: boolean,
): EffortTrackKind {
  if (isMax && fast) return "fusion";
  if (isMax) return "supercharged";
  if (fast) return "fast";
  return "plain";
}

/**
 * 帧间隔：只有单独的最高档降到 30fps，其余动效（速度线与闪电需要跟手）保持
 * 60fps。上限由调用方按 `requestAnimationFrame` 的实际节拍再收敛一次。
 */
export function trackFrameIntervalMs(kind: EffortTrackKind): number {
  return kind === "supercharged" ? 1000 / 30 : 1000 / 60;
}

/**
 * 稳定的伪随机值，落在 `[0, 1)`。
 *
 * `fract(sin(dot()))` 哈希：粒子的编号与属性固定，因此每帧重建也得到同一组
 * 参数，无需保存状态即可让粒子长得一样。
 */
export function noise(index: number, trait: number): number {
  const value = Math.sin(index * 12.9898 + trait * 78.233) * 43758.5453;
  return value - Math.floor(value);
}

/** 在填充两端各 `EDGE_FADE` 点内把元素淡出，避免元素撞上胶囊边界。 */
export function edgeFade(x: number, width: number): number {
  return Math.min(1, Math.max(0, x / EDGE_FADE), Math.max(0, (width - x) / EDGE_FADE));
}

export interface EffortRgb {
  r: number;
  g: number;
  b: number;
}

const HEX_COLOR = /^#([0-9a-f]{3,8})$/i;
const RGB_COLOR = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)/i;

/** 解析 `#rgb`/`#rrggbb`/`rgb()` 形式的颜色；CSS 变量读到 `color-mix()` 时返回 null。 */
export function parseColor(value: string): EffortRgb | null {
  const text = value.trim();
  const hex = HEX_COLOR.exec(text);
  if (hex) {
    const digits = hex[1]!;
    if (digits.length === 3 || digits.length === 4) {
      return {
        r: parseInt(digits[0]! + digits[0]!, 16),
        g: parseInt(digits[1]! + digits[1]!, 16),
        b: parseInt(digits[2]! + digits[2]!, 16),
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

/** 把解析出的颜色按给定不透明度写成 `rgba()` 字面量。 */
export function withAlpha(color: EffortRgb, alpha: number): string {
  const clamp = (n: number) => Math.max(0, Math.min(255, Math.round(n)));
  return `rgba(${clamp(color.r)}, ${clamp(color.g)}, ${clamp(color.b)}, ${alpha})`;
}

/** 从元素的计算样式读取画布用色，使 DOM 与画布共用同一组令牌。 */
export function readEffortTrackPalette(
  styles: Pick<CSSStyleDeclaration, "getPropertyValue">,
): EffortTrackPalette {
  const read = (name: string, fallback: string) => {
    const value = styles.getPropertyValue(name).trim();
    return value || fallback;
  };
  return {
    brand: read("--effort-brand", EFFORT_TRACK_FALLBACK_PALETTE.brand),
    fast: read("--effort-fast", EFFORT_TRACK_FALLBACK_PALETTE.fast),
    spark: read("--effort-spark", EFFORT_TRACK_FALLBACK_PALETTE.spark),
  };
}

/** 读取一个元素上的画布颜色令牌（`window` 缺失时用兜底，便于测试）。 */
export function readEffortPaletteFromElement(
  element: Element | null,
): EffortTrackPalette {
  if (typeof window === "undefined" || !element) {
    return EFFORT_TRACK_FALLBACK_PALETTE;
  }
  return readEffortTrackPalette(window.getComputedStyle(element));
}

/** 画布需要的最小能力，便于在无 DOM 的测试里替换成记录桩。 */
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
  | "fill"
  | "stroke"
  | "save"
  | "restore"
  | "clip"
  | "createLinearGradient"
  | "createRadialGradient"
>;

/** 按当前外观绘制一帧动效；`plain` 不产生任何绘制。 */
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
    drawStreaks(ctx, size, time);
    drawBolts(ctx, size, time, palette.fast);
    drawSheen(ctx, size, time, palette.spark);
  }
}

/** 沿填充漂移的粒子，各自有高度、大小、速度与闪烁频率。 */
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
    // 每颗粒子的闪烁频率不同，整片不会同相呼吸。
    const shimmer = 0.6 + 0.4 * Math.sin(time * (2 + 3 * noise(seed, 6)) + seed);
    const opacity = brightness * shimmer * edgeFade(x, size.width);
    if (opacity <= 0.01) continue;
    ctx.fillStyle = withAlpha(rgb, opacity);
    ctx.beginPath();
    ctx.arc(x, y, radius, 0, TAU);
    ctx.fill();
  }
}

/** 冲向旋钮的细速度线，头部明亮、拖尾透明。 */
function drawStreaks(
  ctx: TrackCanvas,
  size: EffortTrackSize,
  time: number,
): void {
  const white = { r: 255, g: 255, b: 255 };
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
    const brightness = (0.35 + 0.55 * noise(seed, 6)) * edgeFade(x + length, width);
    if (brightness <= 0.02) continue;
    const gradient = ctx.createLinearGradient(x, y, x + length, y);
    gradient.addColorStop(0, withAlpha(white, 0));
    gradient.addColorStop(1, withAlpha(white, brightness));
    ctx.fillStyle = gradient;
    roundedRectPath(ctx, x, y - thickness / 2, length, thickness, thickness / 2);
    ctx.fill();
  }
}

/** 两个各自打点的闪电：每 1.4~2.3 秒在某处闪 0.26 秒。 */
function drawBolts(
  ctx: TrackCanvas,
  size: EffortTrackSize,
  time: number,
  color: string,
): void {
  const rgb = parseColor(color);
  if (!rgb) return;
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
    // 每个周期换一处落点，避免闪电固定在同一个位置。
    const cycleSeed = seed + cycle * 7.31;
    const x = 12 + (width - 24) * noise(cycleSeed, 3);
    if (x <= 6 || x >= width - 6) continue;
    const lean = 3 + 3 * noise(cycleSeed, 4);

    ctx.save();
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    // 辉光用 shadowBlur，而不是 Safari 18 才支持的 ctx.filter。
    ctx.shadowColor = withAlpha(rgb, 0.9 * intensity);
    ctx.shadowBlur = 6;
    ctx.strokeStyle = withAlpha(rgb, 0.9 * intensity);
    ctx.lineWidth = 4;
    boltPath(ctx, x, lean, height);
    ctx.stroke();
    ctx.shadowBlur = 0;
    ctx.strokeStyle = withAlpha({ r: 255, g: 255, b: 255 }, intensity);
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

/** 斜切光带，每隔 SHEEN_PERIOD 扫过整个填充。 */
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

/**
 * 融合：品牌色从近端涌入、快速色从远端涌入，在漂移的白热接缝处相遇。
 * 接缝两侧互相渗入对方颜色的等离子带，接缝飞出的火花由白冷却成落脚侧的颜色。
 */
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
  // 两个不同频率的正弦叠加：接缝缓慢漂移，但不显得机械。
  const seam = width * (0.5 + 0.13 * Math.sin(time * 0.7) + 0.05 * Math.sin(time * 1.9 + 1));
  const pulse = 0.5 + 0.5 * Math.sin(time * 2.4);

  // 两侧各守住自己的一半，只在接缝附近短暂融合，缝心是白色。
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

/** 等离子带：对方的颜色各自伸进本侧，加色混合，越靠近接缝越亮。 */
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
      // 只在本侧可见：近端带画在接缝左边，远端带画在右边。
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
          amplitude * Math.sin((x / wavelength) * TAU + (toNear ? streamPhase : -streamPhase));
        if (step === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      ctx.stroke();
      ctx.restore();
    }
  }
}

/** 接缝飞出的火花：离开时是白色，飞远后冷却成落脚侧的颜色。 */
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
    const y = height * (0.18 + 0.64 * noise(seed, 5)) + 5 * Math.sin(age * 8 + seed);
    const radius = 0.7 + 1.5 * (1 - progress);
    const opacity = (1 - progress) * (0.5 + 0.5 * noise(seed, 6)) * edgeFade(x, width);
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

/** 接缝的白热核心：呼吸的光晕盘加一根亮线。 */
function drawSeamCore(
  ctx: TrackCanvas,
  size: EffortTrackSize,
  seam: number,
  pulse: number,
): void {
  const height = size.height;
  const white = { r: 255, g: 255, b: 255 };
  const coreWidth = 9 + 8 * pulse;
  const glow = ctx.createRadialGradient(seam, height / 2, 0, seam, height / 2, coreWidth);
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

/** `roundRect` 在较旧的 WebKit 上缺失，这里用 `arcTo` 手绘圆角矩形。 */
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
