/**
 * 资源面板代码预览，使用 highlight.js 渲染语法高亮。
 * with light/dark themes bound to `data-theme` on documentElement.
 */

import { useEffect, useMemo, useState } from "react";
import DOMPurify from "dompurify";
import hljs from "highlight.js/lib/core";

import { languageFromFileName } from "@/lib/codeLang";
import {
  ensureHighlightLanguages,
  normalizeHighlightLanguage,
} from "@/lib/highlightLanguages";
import { cn } from "@/lib/utils";

// Themes: Atom One Dark / One Light (scoped in code-preview.css)
import "@/styles/code-preview.css";

export interface CodePreviewProps {
  code: string;
  /** File name for language detection (preferred). */
  fileName?: string;
  /** Explicit highlight.js language id. */
  language?: string;
  className?: string;
  /** Optional footer note (e.g. truncated). */
  footer?: string | null;
}

function readDocTheme(): "light" | "dark" {
  if (typeof document === "undefined") return "dark";
  const t = document.documentElement.getAttribute("data-theme");
  return t === "light" ? "light" : "dark";
}

export function CodePreview({
  code,
  fileName,
  language,
  className,
  footer,
}: CodePreviewProps) {
  ensureHighlightLanguages();

  const [theme, setTheme] = useState<"light" | "dark">(readDocTheme);

  useEffect(() => {
    const root = document.documentElement;
    const sync = () => setTheme(readDocTheme());
    sync();
    const mo = new MutationObserver(sync);
    mo.observe(root, { attributes: true, attributeFilter: ["data-theme"] });
    return () => mo.disconnect();
  }, []);

  const lang = useMemo(() => {
    if (language && language !== "auto") return language;
    if (fileName) return languageFromFileName(fileName);
    return "plaintext";
  }, [language, fileName]);

  const html = useMemo(() => {
    try {
      const normalized = normalizeHighlightLanguage(lang);
      if (normalized && hljs.getLanguage(normalized)) {
        return hljs.highlight(code, {
          language: normalized,
          ignoreIllegals: true,
        })
          .value;
      }
      return hljs.highlightAuto(code).value;
    } catch {
      // Escape minimal HTML if highlight fails
      return code
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;");
    }
  }, [code, lang]);

  const lines = useMemo(() => {
    // Keep trailing newline as empty last line for gutter count
    const parts = code.split("\n");
    if (parts.length > 0 && parts[parts.length - 1] === "") parts.pop();
    return Math.max(parts.length, 1);
  }, [code]);

  return (
    <div
      className={cn(
        "rp-code",
        theme === "light" ? "rp-code--light" : "rp-code--dark",
        className,
      )}
      data-language={lang}
    >
      <div className="rp-code__scroll">
        <div className="rp-code__gutter" aria-hidden>
          {Array.from({ length: lines }, (_, i) => (
            <span key={i} className="rp-code__ln">
              {i + 1}
            </span>
          ))}
        </div>
        <pre className="rp-code__pre">
          <code
            className={`hljs language-${lang}`}
            dangerouslySetInnerHTML={{ __html: DOMPurify.sanitize(html) }}
          />
        </pre>
      </div>
      {footer ? <div className="rp-code__footer">{footer}</div> : null}
    </div>
  );
}
