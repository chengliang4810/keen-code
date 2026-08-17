import { memo, useRef, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  IncrementalMarkdownState,
  requiresFullMarkdownParse,
  type MarkdownBlockPosition,
} from "@/lib/incrementalMarkdown";

interface PositionedNode {
  position?: {
    start?: { offset?: number };
    end?: { offset?: number };
  };
}

interface MarkdownRoot {
  children?: PositionedNode[];
}

const settledPlugins = [remarkGfm];

function captureBlockPositions(
  capture: (positions: readonly MarkdownBlockPosition[]) => void,
) {
  return function capturePlugin() {
    return (root: MarkdownRoot) => {
      const positions: MarkdownBlockPosition[] = [];
      for (const node of root.children ?? []) {
        const start = node.position?.start?.offset;
        const end = node.position?.end?.offset;
        if (start === undefined || end === undefined) {
          capture([]);
          return;
        }
        positions.push({ start, end });
      }
      capture(positions);
    };
  };
}

const MarkdownSegment = memo(function MarkdownSegment({
  source,
  components,
  capture,
}: {
  source: string;
  components: Components;
  capture?: (positions: readonly MarkdownBlockPosition[]) => void;
}) {
  const plugins = capture
    ? [remarkGfm, captureBlockPositions(capture)]
    : settledPlugins;
  return (
    <ReactMarkdown remarkPlugins={plugins} components={components}>
      {source}
    </ReactMarkdown>
  );
});

/**
 * A settled message gets one canonical full parse. While streaming, memoized
 * prefix segments stay mounted and only the two-block source tail is parsed.
 */
export function IncrementalMarkdown({
  source,
  streaming,
  components,
  disabled = false,
}: {
  source: string;
  streaming: boolean;
  components: Components;
  /** Full parsing is required while in-chat find counts across the document. */
  disabled?: boolean;
}): ReactNode {
  const stateRef = useRef<IncrementalMarkdownState | null>(null);

  if (!streaming || disabled || requiresFullMarkdownParse(source)) {
    stateRef.current = null;
    return (
      <MarkdownSegment source={source} components={components} />
    );
  }

  if (stateRef.current === null) {
    stateRef.current = new IncrementalMarkdownState();
  }
  const state = stateRef.current;
  const frame = state.update(source);
  const generationKey = `markdown-stream-${frame.generation}`;

  return (
    <>
      {frame.frozen.map((segment) => (
        <MarkdownSegment
          key={`${generationKey}-${segment.key}`}
          source={segment.source}
          components={components}
        />
      ))}
      <MarkdownSegment
        key={`${generationKey}-${frame.tail.key}`}
        source={frame.tail.source}
        components={components}
        capture={(positions) =>
          state.recordTailPositions(
            frame.tail.key,
            frame.tail.source,
            positions,
          )
        }
      />
    </>
  );
}
