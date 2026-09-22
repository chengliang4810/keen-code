import { CopyButton } from "@appica/ui-react/copy-button";
import { Toggle } from "@appica/ui-react/toggle";
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type MutableRefObject,
  type ReactNode,
} from "react";
import {
  IconActivity,
  IconCode,
  IconDatabase,
  IconFileDiff,
  IconFileText,
  IconMaximize,
  IconTerminal,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Tip } from "@/components/ui/tooltip";
import { highlightChatCode } from "@/lib/chatCodeHighlight";
import { cn } from "@/lib/utils";
import {
  resolveCodeBlockDescriptor,
  shouldRenderMermaidCodeBlock,
  splitHighlightedHtml,
  type CodeBlockIconKind,
} from "./codeBlockMeta";
import {
  DiagramPreviewDialog,
  type DiagramPreviewLabels,
} from "./DiagramPreviewDialog";
import { MermaidBlock, type MermaidBlockLabels } from "./MermaidBlock";

/** 消息代码块：保留行级高亮与复制操作，默认不展示文件名和行号。 */

function extractText(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(extractText).join("");
  if (typeof node === "object" && "props" in node) {
    const p = node as { props?: { children?: ReactNode } };
    return extractText(p.props?.children);
  }
  return "";
}

/** 只把稳定的图标类别映射到现有图标，不在渲染层复制语言识别表。 */
function CodeBlockIcon({ kind }: { kind: CodeBlockIconKind }) {
  switch (kind) {
    case "data":
      return <IconDatabase size={14} />;
    case "diff":
      return <IconFileDiff size={14} />;
    case "document":
      return <IconFileText size={14} />;
    case "diagram":
      return <IconActivity size={14} />;
    case "shell":
      return <IconTerminal size={14} />;
    case "code":
    default:
      return <IconCode size={14} />;
  }
}

function isLineInRange(
  line: number,
  range: { startLine: number; endLine: number } | null | undefined,
): boolean {
  if (!range) return false;
  const start = Math.min(range.startLine, range.endLine);
  const end = Math.max(range.startLine, range.endLine);
  return line >= start && line <= end;
}

function CodeLines({
  text,
  highlightedHtml,
  focusedRange,
  markedLines,
  lineRefs,
}: {
  text: string;
  highlightedHtml: string | null;
  focusedRange?: { startLine: number; endLine: number } | null;
  markedLines?: readonly number[];
  lineRefs: MutableRefObject<Map<number, HTMLSpanElement>>;
}) {
  const lines = text.split("\n");
  const highlightedLines = highlightedHtml
    ? splitHighlightedHtml(highlightedHtml)
    : null;
  const marked = useMemo(
    () =>
      new Set(
        (markedLines ?? []).filter(
          (line) => Number.isInteger(line) && line > 0,
        ),
      ),
    [markedLines],
  );

  return (
    <code className={cn("chat-code__code", highlightedHtml && "hljs")}>
      {lines.map((line, index) => {
        const lineNumber = index + 1;
        const focused = isLineInRange(lineNumber, focusedRange);
        const markedLine = marked.has(lineNumber);
        const htmlLine = highlightedLines?.[index] ?? "";
        return (
          <span
            key={lineNumber}
            ref={(element) => {
              if (element) lineRefs.current.set(lineNumber, element);
              else lineRefs.current.delete(lineNumber);
            }}
            className={cn(
              "chat-code__line",
              focused && "is-focused",
              markedLine && "is-marked",
            )}
            data-line={lineNumber}
            data-focused={focused ? "true" : undefined}
            data-marked={markedLine ? "true" : undefined}
          >
            {highlightedHtml ? (
              <span
                className="chat-code__line-content"
                dangerouslySetInnerHTML={{ __html: htmlLine }}
              />
            ) : (
              <span className="chat-code__line-content">{line}</span>
            )}
          </span>
        );
      })}
    </code>
  );
}

export function CodeBlock({
  language,
  children,
  wrapLabel = "Wrap",
  unwrapLabel = "No wrap",
  copyLabel = "Copy",
  langLabel,
  highlight = false,
  previewLabel = "Open preview",
  previewTitle = "Diagram preview",
  previewDescription = "Interactive diagram preview",
  previewLoading = "Rendering diagram…",
  previewCloseLabel = "Close",
  previewZoomInLabel = "Zoom in",
  previewZoomOutLabel = "Zoom out",
  previewResetLabel = "Reset zoom",
  focusedRange,
  focusRequestId,
  markedLines,
}: {
  language?: string;
  children: ReactNode;
  wrapLabel?: string;
  unwrapLabel?: string;
  copyLabel?: string;
  langLabel?: string;
  /** Only true after the stream settles; live fences remain plain text. */
  highlight?: boolean;
  /** Render a line range as focused and scroll to it when the request changes. */
  focusedRange?: { startLine: number; endLine: number } | null;
  focusRequestId?: string;
  /** Mark diagnostic lines without changing the code text. */
  markedLines?: readonly number[];
  previewLabel?: string;
  previewTitle?: string;
  previewDescription?: string;
  previewLoading?: string;
  previewCloseLabel?: string;
  previewZoomInLabel?: string;
  previewZoomOutLabel?: string;
  previewResetLabel?: string;
}) {
  const [wrap, setWrap] = useState(false);
  const [mermaidSvg, setMermaidSvg] = useState<string | null>(null);
  const [previewOpen, setPreviewOpen] = useState(false);
  const lineRefs = useRef(new Map<number, HTMLSpanElement>());
  const lang = (language || "text").replace(/^language-/, "") || "text";
  const text = extractText(children).replace(/^\n+|\n+$/g, "");
  const descriptor = useMemo(() => resolveCodeBlockDescriptor(lang), [lang]);
  const highlightedHtml = useMemo(
    () => (highlight ? highlightChatCode(text, lang) : null),
    [highlight, lang, text],
  );
  const renderMermaid = highlight && shouldRenderMermaidCodeBlock(lang, text);
  const mermaidLabels: MermaidBlockLabels = useMemo(
    () => ({ loading: previewLoading, diagram: previewTitle }),
    [previewLoading, previewTitle],
  );
  const previewLabels: DiagramPreviewLabels = useMemo(
    () => ({
      title: previewTitle,
      description: previewDescription,
      close: previewCloseLabel,
      zoomIn: previewZoomInLabel,
      zoomOut: previewZoomOutLabel,
      reset: previewResetLabel,
    }),
    [
      previewCloseLabel,
      previewDescription,
      previewResetLabel,
      previewTitle,
      previewZoomInLabel,
      previewZoomOutLabel,
    ],
  );

  useEffect(() => {
    if (!focusRequestId || !focusedRange) return;
    const firstLine = Math.min(focusedRange.startLine, focusedRange.endLine);
    lineRefs.current.get(firstLine)?.scrollIntoView?.({ block: "nearest" });
  }, [focusRequestId, focusedRange]);

  useEffect(() => {
    if (!renderMermaid) {
      setMermaidSvg(null);
      setPreviewOpen(false);
    }
  }, [renderMermaid]);

  return (
    <>
      <div className="chat-code">
        <div className="chat-code__bar">
          <span
            className="chat-code__lang"
            title={langLabel ?? descriptor.language}
          >
            <span
              className="chat-code__icon"
              data-code-block-icon={descriptor.iconKind}
              aria-hidden="true"
            >
              <CodeBlockIcon kind={descriptor.iconKind} />
            </span>
            <span className="chat-code__language-label">
              {langLabel ?? descriptor.language}
            </span>
          </span>
          <div className="chat-code__bar-actions">
            {renderMermaid ? (
              <Tip label={previewLabel}>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-md"
                  aria-label={previewLabel}
                  disabled={!mermaidSvg}
                  onClick={() => setPreviewOpen(true)}
                >
                  <IconMaximize size={15} />
                </Button>
              </Tip>
            ) : null}
            <Tip label={wrap ? unwrapLabel : wrapLabel}>
              <Toggle
                aria-label={wrap ? unwrapLabel : wrapLabel}
                pressed={wrap}
                onPressedChange={setWrap}
                render={
                  <Button variant="ghost" size="icon-md">
                    <span className="chat-code__wrap-icon" aria-hidden>
                      ↵
                    </span>
                  </Button>
                }
              />
            </Tip>
            <CopyButton
              value={text}
              size="md"
              label={copyLabel}
              copiedLabel="OK"
              timeout={2000}
            />
          </div>
        </div>
        {renderMermaid ? (
          <MermaidBlock
            code={text}
            labels={mermaidLabels}
            onPreviewSvgChange={setMermaidSvg}
          />
        ) : (
          <pre className={cn("chat-code__pre", wrap && "is-wrap")}>
            <CodeLines
              text={text}
              highlightedHtml={highlightedHtml}
              focusedRange={focusedRange}
              markedLines={markedLines}
              lineRefs={lineRefs}
            />
          </pre>
        )}
      </div>
      {renderMermaid ? (
        <DiagramPreviewDialog
          open={previewOpen}
          svg={mermaidSvg}
          labels={previewLabels}
          onOpenChange={setPreviewOpen}
        />
      ) : null}
    </>
  );
}
