/**
 * Soft buffer for pure-text streaming first paint (CodePilot-style).
 * Holds initial text until word/char threshold or max wait; code fences bypass.
 */

export const SOFT_BUFFER_WORD_THRESHOLD = 12;
export const SOFT_BUFFER_CHAR_THRESHOLD = 40;
export const SOFT_BUFFER_MAX_MS = 2500;

/** Structured fences that must bypass the soft buffer for live preview. */
const STRUCTURED_FENCE_RE =
  /```(show-widget|batch-plan|image-gen|tsx|jsx|html|json|mermaid)/i;

export interface SoftBufferState {
  bypassed: boolean;
  /** epoch ms when first non-empty content arrived */
  firstContentAt: number | null;
  released: string;
}

export function createSoftBufferState(): SoftBufferState {
  return { bypassed: false, firstContentAt: null, released: "" };
}

function wordCount(s: string): number {
  return s.split(/\s+/).filter(Boolean).length;
}

/**
 * Pure step: given raw target and time, return what should be displayed.
 * When not streaming, always returns full target.
 */
export function stepSoftBuffer(input: {
  state: SoftBufferState;
  raw: string;
  streaming: boolean;
  nowMs: number;
  wordThreshold?: number;
  charThreshold?: number;
  maxMs?: number;
}): { state: SoftBufferState; displayed: string } {
  const {
    raw,
    streaming,
    nowMs,
    wordThreshold = SOFT_BUFFER_WORD_THRESHOLD,
    charThreshold = SOFT_BUFFER_CHAR_THRESHOLD,
    maxMs = SOFT_BUFFER_MAX_MS,
  } = input;
  let state = input.state;

  if (!streaming) {
    return {
      state: { bypassed: true, firstContentAt: state.firstContentAt, released: raw },
      displayed: raw,
    };
  }

  if (!raw) {
    return { state, displayed: state.released };
  }

  if (state.firstContentAt == null && raw.trim()) {
    state = { ...state, firstContentAt: nowMs };
  }

  const shouldBypass =
    state.bypassed ||
    STRUCTURED_FENCE_RE.test(raw) ||
    wordCount(raw) >= wordThreshold ||
    raw.length >= charThreshold ||
    (state.firstContentAt != null && nowMs - state.firstContentAt >= maxMs);

  if (shouldBypass) {
    return {
      state: { ...state, bypassed: true, released: raw },
      displayed: raw,
    };
  }

  // Still buffering — show nothing (or previous released empty).
  return { state, displayed: state.released };
}
