/**
 * 资源面板代码预览:Shiki token 上色。
 * 明暗主题经 `--shiki-light/--shiki-dark` CSS 变量跟随 documentElement 的
 * `data-theme` 切换(样式在 code-preview.css),组件本身不再观察主题。
 *
 * 迁移说明:旧 highlight.js 的 `highlightAuto` 未知语言自动检测在切换到
 * Shiki 时移除(与聊天代码块同一高亮器);未识别语言按纯文本渲染。
 */

import { useEffect, useMemo, useState } from "react";
import { languageFromFileName } from "@/lib/codeLang";
import {
  highlightChatCodeLines,
  type ChatCodeLine,
} from "@/lib/shikiChatHighlighter";
import { cn } from "@/lib/utils";

import "@/styles/code-preview.css";

export interface CodePreviewProps {
  code: string;
  /** File name for language detection (preferred). */
  fileName?: string;
  /** Explicit Shiki-compatible language id. */
  language?: string;
  className?: string;
  /** Optional footer note (e.g. truncated). */
  footer?: string | null;
}

export function CodePreview({
  code,
  fileName,
  language,
  className,
  footer,
}: CodePreviewProps) {
  const lang = useMemo(() => {
    if (language && language !== "auto") return language;
    if (fileName) return languageFromFileName(fileName);
    return "plaintext";
  }, [language, fileName]);

  const [tokenLines, setTokenLines] = useState<ChatCodeLine[] | null>(null);
  useEffect(() => {
    let active = true;
    setTokenLines(null);
    highlightChatCodeLines(code, lang, (lines) => {
      if (active) setTokenLines(lines);
    });
    return () => {
      active = false;
    };
  }, [code, lang]);

  const displayLines = useMemo(() => {
    const parts = code.split("\n");
    // Keep trailing newline as empty last line for gutter count
    if (parts.length > 0 && parts[parts.length - 1] === "") parts.pop();
    if (parts.length === 0) parts.push("");
    return parts;
  }, [code]);

  return (
    <div className={cn("rp-code", className)} data-language={lang}>
      <div className="rp-code__scroll">
        <div className="rp-code__gutter" aria-hidden>
          {displayLines.map((_, index) => (
            <span key={index} className="rp-code__ln">
              {index + 1}
            </span>
          ))}
        </div>
        <pre className="rp-code__pre">
          <code className="rp-code__code">
            {displayLines.map((line, index) => {
              const tokens = tokenLines?.[index];
              return (
                <span key={index} className="rp-code__line">
                  {tokens ? (
                    tokens.map((token, tokenIndex) => (
                      <span
                        key={tokenIndex}
                        className="rp-code__token"
                        style={token.style}
                      >
                        {token.content}
                      </span>
                    ))
                  ) : (
                    <span className="rp-code__token">{line}</span>
                  )}
                </span>
              );
            })}
          </code>
        </pre>
      </div>
      {footer ? <div className="rp-code__footer">{footer}</div> : null}
    </div>
  );
}
