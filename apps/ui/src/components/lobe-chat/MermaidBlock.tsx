import DOMPurify from "dompurify";
import { useEffect, useId, useState } from "react";

export interface MermaidBlockLabels {
  loading: string;
  diagram: string;
}

type MermaidRenderState =
  | { status: "source" }
  | { status: "loading" }
  | { status: "ready"; svg: string }
  | { status: "error" };

type MermaidModule = typeof import("mermaid");

let mermaidModulePromise: Promise<MermaidModule> | null = null;
let mermaidRenderQueue = Promise.resolve();

function loadMermaid(): Promise<MermaidModule> {
  mermaidModulePromise ??= import("mermaid");
  return mermaidModulePromise;
}

function enqueueMermaidRender<T>(task: () => Promise<T>): Promise<T> {
  const run = mermaidRenderQueue.then(task, task);
  mermaidRenderQueue = run.then(
    () => undefined,
    () => undefined,
  );
  return run;
}

function hashCode(value: string): string {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0).toString(36);
}

function browserTheme(): "dark" | "default" {
  if (typeof document === "undefined") return "dark";
  return document.documentElement.getAttribute("data-theme") === "light"
    ? "default"
    : "dark";
}

function sanitizeMermaidSvg(svg: string): string {
  return DOMPurify.sanitize(svg, {
    USE_PROFILES: { svg: true, svgFilters: true },
  });
}

/**
 * Mermaid's default htmlLabels render flowchart node text as HTML inside
 * foreignObject. The sanitizer below is SVG-profile-only and would strip
 * that whole subtree, leaving label-less boxes. Flowchart-family diagrams
 * honor `htmlLabels: false` and then draw plain SVG <text>/<tspan> labels,
 * which survive sanitization and stay inside the strict boundary.
 *//**
 * Mermaid is intentionally client-only and settled-message-only. During
 * streaming, the parent keeps using the ordinary code path so partial syntax
 * never starts an async renderer or replaces the first visible delta.
 */
export function MermaidBlock({
  code,
  labels,
  onPreviewSvgChange,
}: {
  code: string;
  labels: MermaidBlockLabels;
  onPreviewSvgChange?: (svg: string | null) => void;
}) {
  const renderId = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const [state, setState] = useState<MermaidRenderState>({ status: "source" });
  const theme = browserTheme();
  const trimmedCode = code.trim();

  useEffect(() => {
    let cancelled = false;
    if (!trimmedCode) {
      setState({ status: "source" });
      onPreviewSvgChange?.(null);
      return;
    }

    setState({ status: "loading" });
    onPreviewSvgChange?.(null);
    const id = `keencode-mermaid-${renderId}-${hashCode(`${theme}:${trimmedCode}`)}`;

    void enqueueMermaidRender(async () => {
      const { default: mermaid } = await loadMermaid();
      mermaid.initialize({
        startOnLoad: false,
        securityLevel: "strict",
        htmlLabels: false,
        theme,
      });
      return mermaid.render(id, trimmedCode);
    })
      .then(({ svg }) => {
        if (cancelled) return;
        const safeSvg = sanitizeMermaidSvg(svg);
        if (!safeSvg.trim()) {
          setState({ status: "error" });
          onPreviewSvgChange?.(null);
          return;
        }
        setState({ status: "ready", svg: safeSvg });
        onPreviewSvgChange?.(safeSvg);
      })
      .catch(() => {
        if (cancelled) return;
        // Model output can be temporarily incomplete or invalid. Source is a
        // useful, safe fallback and remains copyable instead of showing a
        // broken diagram surface.
        setState({ status: "error" });
        onPreviewSvgChange?.(null);
      });

    return () => {
      cancelled = true;
      onPreviewSvgChange?.(null);
    };
  }, [onPreviewSvgChange, renderId, theme, trimmedCode]);

  if (state.status !== "ready") {
    return (
      <div className="chat-mermaid" data-mermaid-status={state.status}>
        {state.status === "loading" ? (
          <div className="chat-mermaid__loading" role="status" aria-live="polite">
            {labels.loading}
          </div>
        ) : null}
        <pre className="chat-code__pre chat-mermaid__source">
          <code>{code}</code>
        </pre>
      </div>
    );
  }

  return (
    <div className="chat-mermaid" data-mermaid-status="ready">
      <div
        className="chat-mermaid__svg"
        role="img"
        aria-label={labels.diagram}
        dangerouslySetInnerHTML={{ __html: state.svg }}
      />
    </div>
  );
}
