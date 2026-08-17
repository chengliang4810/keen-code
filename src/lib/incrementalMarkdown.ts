/**
 * Block-level state for an append-only Markdown stream.
 *
 * ReactMarkdown exposes source positions to remark plugins. The renderer feeds
 * those positions back here after each tail parse; on the next append we can
 * freeze every block except the last two and only parse the remaining tail.
 */

const UNSTABLE_TAIL_BLOCKS = 2;

/**
 * 引用定义作用于整篇 Markdown，不能把定义和引用拆成独立文档解析。
 * 这类少见的全局构造保守回退一次全文解析，保证流式结果与最终结果一致。
 */
export function requiresFullMarkdownParse(source: string): boolean {
  return /^[\t ]{0,3}\[[^\]\n]+\]:/m.test(source);
}

export interface MarkdownBlockPosition {
  start: number;
  end: number;
}

export interface FrozenMarkdownSegment {
  /** Absolute source offset, stable for the lifetime of one stream generation. */
  key: number;
  source: string;
}

export interface IncrementalMarkdownFrame {
  frozen: readonly FrozenMarkdownSegment[];
  tail: FrozenMarkdownSegment;
  generation: number;
}

/**
 * Keep parsed prefix blocks immutable while an assistant message only grows.
 * Non-append updates start a new generation and discard all frozen segments.
 */
export class IncrementalMarkdownState {
  private previousText = "";
  private tailStart = 0;
  private frozen: FrozenMarkdownSegment[] = [];
  private tailPositions: MarkdownBlockPosition[] = [];
  private generation = 0;
  private cached: IncrementalMarkdownFrame | null = null;

  update(text: string): IncrementalMarkdownFrame {
    if (this.cached && text === this.previousText) return this.cached;

    if (!text.startsWith(this.previousText)) {
      this.previousText = "";
      this.tailStart = 0;
      this.frozen = [];
      this.tailPositions = [];
      this.generation += 1;
    }

    // Positions describe the previous frame's tail. Appending can reshape the
    // parse frontier, so retain two top-level blocks as a safety margin.
    const freezeCount = Math.max(
      0,
      this.tailPositions.length - UNSTABLE_TAIL_BLOCKS,
    );
    if (freezeCount > 0) {
      const cutEnd = this.tailPositions[freezeCount - 1]?.end;
      const previousTailLength = this.previousText.length - this.tailStart;
      if (
        cutEnd !== undefined &&
        cutEnd > 0 &&
        cutEnd <= previousTailLength
      ) {
        this.frozen.push({
          key: this.tailStart,
          source: this.previousText.slice(
            this.tailStart,
            this.tailStart + cutEnd,
          ),
        });
        this.tailStart += cutEnd;
      }
    }

    this.previousText = text;
    this.tailPositions = [];
    this.cached = {
      frozen: [...this.frozen],
      tail: { key: this.tailStart, source: text.slice(this.tailStart) },
      generation: this.generation,
    };
    return this.cached;
  }

  /** Record top-level block offsets produced by the current tail parse. */
  recordTailPositions(
    tailKey: number,
    tailSource: string,
    positions: readonly MarkdownBlockPosition[],
  ): void {
    // Ignore a stale render (possible under concurrent React rendering).
    if (
      tailKey !== this.tailStart ||
      tailSource !== this.previousText.slice(this.tailStart)
    ) {
      return;
    }

    const valid: MarkdownBlockPosition[] = [];
    let previousEnd = 0;
    for (const position of positions) {
      if (
        !Number.isInteger(position.start) ||
        !Number.isInteger(position.end) ||
        position.start < 0 ||
        position.end <= position.start ||
        position.start < previousEnd ||
        position.end > tailSource.length
      ) {
        return;
      }
      valid.push({ start: position.start, end: position.end });
      previousEnd = position.end;
    }
    this.tailPositions = valid;
  }
}
