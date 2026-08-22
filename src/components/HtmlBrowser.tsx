/**
 * Local HTML preview for the resource pane.
 *
 * Nested Tauri WebViews are possible but heavy (positioning, z-index, lifecycle).
 * WKWebView also blocks `file://` inside the main app iframe → blank page.
 *
 * Reliable approach for local reports (usually self-contained):
 * load HTML text via host/asset and render with `srcDoc` (scripts work, full-bleed).
 */

import { useEffect, useMemo, useState } from "react";
import { isTauri } from "@/lib/api";
import { createT, type Locale } from "@/i18n";
import { localizeUiError } from "@/lib/session";

export interface HtmlBrowserProps {
  title?: string;
  /** Absolute filesystem path (for fetch fallback). */
  absolutePath?: string | null;
  /** Full HTML document from host read (preferred). */
  html?: string | null;
  className?: string;
  locale: Locale;
}

async function fetchHtmlText(absolutePath: string): Promise<string> {
  if (!isTauri()) {
    throw new Error("Tauri required to load local HTML");
  }
  const { convertFileSrc } = await import("@tauri-apps/api/core");
  // asset protocol is allowed to read local files from the app webview
  const url = convertFileSrc(absolutePath);
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(`HTTP ${res.status}`);
  }
  return res.text();
}

export function HtmlBrowser({
  title = "HTML",
  absolutePath,
  html,
  className = "",
  locale,
}: HtmlBrowserProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [doc, setDoc] = useState<string>(html?.trim() ? html : "");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(!html?.trim() && !!absolutePath);

  useEffect(() => {
    if (html?.trim()) {
      setDoc(html);
      setError(null);
      setLoading(false);
      return;
    }
    if (!absolutePath) {
      setDoc("");
      setError(tr("resources.htmlEmpty"));
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    void fetchHtmlText(absolutePath)
      .then((text) => {
        if (cancelled) return;
        setDoc(text);
        setLoading(false);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(localizeUiError(e, locale));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [html, absolutePath, locale, tr]);

  if (loading) {
    return (
      <div className={"rp-preview-browser rp-preview-browser--msg " + className}>
        <div className="rp-preview__msg">{tr("resources.loading")}</div>
      </div>
    );
  }

  if (error || !doc) {
    return (
      <div className={"rp-preview-browser rp-preview-browser--msg " + className}>
        <div className="rp-preview__msg" role="alert">
          {error || tr("resources.htmlEmpty")}
        </div>
      </div>
    );
  }

  return (
    <iframe
      className={
        "rp-preview__frame rp-preview__frame--browser " + className
      }
      title={title}
      // 本地 HTML 来自不可信文件，sandbox 隔离同源能力（无 allow-same-origin 时
      // document 域为 opaque，无法触达父级 Tauri IPC）；仅保留 allow-scripts 以
      // 支撑内联脚本（复制按钮）。裁剪的 copy/paste 权限仍由下方 allow 提供。
      sandbox="allow-scripts"
      srcDoc={doc}
      allow="clipboard-read; clipboard-write; fullscreen"
    />
  );
}
