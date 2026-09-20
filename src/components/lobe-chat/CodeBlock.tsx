import { CopyButton } from "@appica/ui-react/copy-button";
import { Button } from "@/components/ui/button";
import { Toggle } from "@appica/ui-react/toggle";
/**
 * Path / code block — soft chrome with a label, wrapping, and copy action.
 */

import { useMemo, useState, type ReactNode } from "react";
import { IconCode } from "@/components/icons";
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
  langLabel,
  highlight = false,
}: {
  language?: string;
  children: ReactNode;
  wrapLabel?: string;
  unwrapLabel?: string;
  copyLabel?: string;
  langLabel?: string;
  /** Only true after the stream settles; live fences remain plain text. */
  highlight?: boolean;
}) {
  const [wrap, setWrap] = useState(false);
  const lang = (language || "text").replace(/^language-/, "") || "text";
  const text = extractText(children).replace(/^\n+|\n+$/g, "");
  const highlightedHtml = useMemo(
    () => (highlight ? highlightChatCode(text, lang) : null),
    [highlight, lang, text],
  );

  return (
    <div className="chat-code">
      <div className="chat-code__bar">
        <span className="chat-code__lang">
          <IconCode size={14} />
          {langLabel ?? lang}
        </span>
        <div className="chat-code__bar-actions">
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
          <CopyButton value={text} label={copyLabel} copiedLabel="OK" timeout={1200} />
        </div>
      </div>
      <pre className={cn("chat-code__pre", wrap && "is-wrap")}>
        {highlightedHtml === null ? (
          /* 渲染 trim 后的文本而非原始 children：fence 内容首部的 \n 会被
           * white-space: pre 渲染成空行。 */
          <code>{text}</code>
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
