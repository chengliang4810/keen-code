import { Button } from "@/components/ui/button";
/**
 * Path / code block — soft chrome with a label, wrapping, and copy action.
 */

import { useMemo, useState, type ReactNode } from "react";
import { IconCheck, IconCopy } from "@/components/icons";
import { Tip } from "@/components/ui/tooltip";
import { highlightChatCode } from "@/lib/chatCodeHighlight";
import { cn } from "@/lib/utils";

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

export function CodeBlock({
  language,
  children,
  wrapLabel = "Wrap",
  unwrapLabel = "No wrap",
  copyLabel = "Copy",
  highlight = false,
}: {
  language?: string;
  children: ReactNode;
  wrapLabel?: string;
  unwrapLabel?: string;
  copyLabel?: string;
  /** Only true after the stream settles; live fences remain plain text. */
  highlight?: boolean;
}) {
  const [wrap, setWrap] = useState(false);
  const [copied, setCopied] = useState(false);
  const lang = (language || "text").replace(/^language-/, "") || "text";
  const text = extractText(children).replace(/\n$/, "");
  const highlightedHtml = useMemo(
    () => (highlight ? highlightChatCode(text, lang) : null),
    [highlight, lang, text],
  );

  const onCopy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch {
      /* ignore */
    }
  };

  return (
    <div className="chat-code">
      <div className="chat-code__bar">
        <span className="chat-code__lang">{lang}</span>
        <div className="chat-code__bar-actions">
          <Tip label={wrap ? unwrapLabel : wrapLabel}>
            <Button
              type="button"
              className={cn("chat-code__btn", wrap && "is-on")}
              aria-label={wrap ? unwrapLabel : wrapLabel}
              aria-pressed={wrap}
              onClick={() => setWrap((v) => !v)}
            >
              <span className="chat-code__wrap-icon" aria-hidden>
                ↵
              </span>
            </Button>
          </Tip>
          <Tip label={copied ? "OK" : copyLabel}>
            <Button
              type="button"
              className={cn("chat-code__btn", copied && "is-copied")}
              aria-label={copyLabel}
              onClick={() => void onCopy()}
            >
              {copied ? <IconCheck size={14} /> : <IconCopy size={14} />}
            </Button>
          </Tip>
        </div>
      </div>
      <pre className={cn("chat-code__pre", wrap && "is-wrap")}>
        {highlightedHtml === null ? (
          <code>{children}</code>
        ) : (
          <code
            className={`hljs language-${lang}`}
            dangerouslySetInnerHTML={{ __html: highlightedHtml }}
          />
        )}
      </pre>
    </div>
  );
}
