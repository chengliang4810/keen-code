/**
 * Rich local document preview:
 * - DOCX → docx-preview (styled Word layout)
 * - XLSX → SheetJS (xlsx) multi-sheet tables
 * - PPTX → limited text fallback + open externally
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { renderAsync } from "docx-preview";
import * as XLSX from "xlsx";
import DOMPurify from "dompurify";
import { fetchPreviewArrayBuffer } from "@/lib/filePreviewSrc";
import { createT, type Locale } from "@/i18n";
import { pathOpen, pathReveal } from "@/lib/api";
import { Tip } from "@/components/ui/tooltip";

export interface OfficeDocumentPreviewProps {
  kind: string;
  absolutePath: string;
  name: string;
  locale: Locale;
  /** Plain-text extract from host (pptx / fallback). */
  textFallback?: string | null;
  errorFromHost?: string | null;
  /**
   * When true (ResourceViewer embed), hide the filename title bar so the host
   * chrome is the only place that shows the file name / open actions.
   */
  embedded?: boolean;
}

type LoadState =
  | { status: "loading" }
  | { status: "error"; message: string }
  | { status: "ready"; buffer: ArrayBuffer };

export function OfficeDocumentPreview({
  kind,
  absolutePath,
  name,
  locale,
  textFallback,
  errorFromHost,
  embedded = false,
}: OfficeDocumentPreviewProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [load, setLoad] = useState<LoadState>({ status: "loading" });
  const [sheetNames, setSheetNames] = useState<string[]>([]);
  const [activeSheet, setActiveSheet] = useState(0);
  const [sheetHtml, setSheetHtml] = useState("");
  const docxRef = useRef<HTMLDivElement>(null);
  const docxScrollRef = useRef<HTMLDivElement>(null);

  /**
   * Force pages to use the pane width so text/tables reflow instead of
   * clipping (docx-preview writes fixed page widths as inline styles).
   */
  const relaxDocxPageWidths = () => {
    const host = docxRef.current;
    if (!host) return;
    host.querySelectorAll<HTMLElement>("section.docx").forEach((sec) => {
      sec.style.setProperty("width", "100%", "important");
      sec.style.setProperty("max-width", "100%", "important");
      sec.style.setProperty("min-width", "0", "important");
      sec.style.setProperty("box-sizing", "border-box", "important");
    });
    const wrap = host.querySelector<HTMLElement>(".docx-wrapper");
    if (wrap) {
      wrap.style.setProperty("width", "100%", "important");
      wrap.style.setProperty("max-width", "100%", "important");
      wrap.style.setProperty("padding", "0", "important");
    }
  };

  useEffect(() => {
    let cancelled = false;
    setLoad({ status: "loading" });
    setSheetNames([]);
    setActiveSheet(0);
    setSheetHtml("");

    if (errorFromHost && !absolutePath) {
      setLoad({ status: "error", message: errorFromHost });
      return;
    }

    // pptx: no mature free browser renderer — prefer text + open
    if (kind === "pptx" || kind === "odf") {
      setLoad({
        status: "error",
        message: tr("office.pptxLimited"),
      });
      return;
    }

    void (async () => {
      try {
        const buf = await fetchPreviewArrayBuffer(absolutePath, kind);
        if (cancelled) return;
        setLoad({ status: "ready", buffer: buf });
      } catch (e) {
        if (cancelled) return;
        setLoad({
          status: "error",
          message: e instanceof Error ? e.message : String(e),
        });
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [absolutePath, kind, errorFromHost, tr]);

  // DOCX render — reflow to pane width (full text, no side clip)
  useEffect(() => {
    if (load.status !== "ready") return;
    if (kind !== "docx" && kind !== "office") return;
    const el = docxRef.current;
    if (!el) return;
    el.innerHTML = "";
    el.style.zoom = "";
    el.style.transform = "";
    let cancelled = false;
    let ro: ResizeObserver | null = null;

    void renderAsync(load.buffer, el, undefined, {
      className: "office-docx-body",
      inWrapper: true,
      // Critical: ignore fixed page width so content uses the container
      // (otherwise Chinese titles / tables overflow and get clipped).
      ignoreWidth: true,
      ignoreHeight: true,
      breakPages: true,
      renderHeaders: true,
      renderFooters: true,
      renderFootnotes: true,
      useBase64URL: true,
      experimental: true,
    })
      .then(() => {
        if (cancelled) return;
        relaxDocxPageWidths();
        requestAnimationFrame(() => {
          if (!cancelled) relaxDocxPageWidths();
        });
        // Images can change layout after load
        el.querySelectorAll("img").forEach((img) => {
          if (img.complete) return;
          img.addEventListener(
            "load",
            () => {
              if (!cancelled) relaxDocxPageWidths();
            },
            { once: true },
          );
        });
        const scroll = docxScrollRef.current;
        if (scroll && typeof ResizeObserver !== "undefined") {
          ro = new ResizeObserver(() => {
            if (!cancelled) relaxDocxPageWidths();
          });
          ro.observe(scroll);
        }
      })
      .catch((e) => {
        if (!cancelled) {
          setLoad({
            status: "error",
            message: e instanceof Error ? e.message : String(e),
          });
        }
      });
    return () => {
      cancelled = true;
      ro?.disconnect();
    };
  }, [load, kind]);

  // XLSX parse
  useEffect(() => {
    if (load.status !== "ready") return;
    if (kind !== "xlsx") return;
    try {
      const wb = XLSX.read(load.buffer, { type: "array" });
      const names = wb.SheetNames;
      setSheetNames(names);
      const idx = 0;
      setActiveSheet(idx);
      const ws = wb.Sheets[names[idx]];
      setSheetHtml(ws ? XLSX.utils.sheet_to_html(ws, { id: "office-sheet" }) : "");
    } catch (e) {
      setLoad({
        status: "error",
        message: e instanceof Error ? e.message : String(e),
      });
    }
  }, [load, kind]);

  const switchSheet = (idx: number) => {
    if (load.status !== "ready" || kind !== "xlsx") return;
    try {
      const wb = XLSX.read(load.buffer, { type: "array" });
      const name = wb.SheetNames[idx];
      const ws = wb.Sheets[name];
      setActiveSheet(idx);
      setSheetHtml(ws ? XLSX.utils.sheet_to_html(ws, { id: "office-sheet" }) : "");
    } catch (e) {
      setLoad({
        status: "error",
        message: e instanceof Error ? e.message : String(e),
      });
    }
  };

  /** 使用系统默认应用打开文档，失败时在文件管理器中定位。 */
  const openExternal = async () => {
    try {
      await pathOpen(absolutePath);
    } catch {
      await pathReveal(absolutePath);
    }
  };

  if (load.status === "loading") {
    return (
      <div className="office-preview office-preview--center">
        <div className="office-preview__status">{tr("office.loading")}</div>
        {!embedded ? (
          <div className="office-preview__sub">{name}</div>
        ) : null}
      </div>
    );
  }

  if (load.status === "error") {
    return (
      <div className="office-preview office-preview--center">
        <div className="office-preview__status">{tr("office.renderFailed")}</div>
        <div className="office-preview__sub">{load.message}</div>
        {textFallback ? (
          <pre className="office-preview__fallback">{textFallback}</pre>
        ) : null}
        <div className="office-preview__actions">
          <button type="button" className="btn btn--solid" onClick={() => void openExternal()}>
            {tr("office.openExternal")}
          </button>
          <button type="button" className="btn btn--ghost" onClick={() => void pathReveal(absolutePath)}>
            {tr("resources.revealFolder")}
          </button>
        </div>
      </div>
    );
  }

  // DOCX — pure document body when embedded (no filename bar)
  if (kind === "docx" || kind === "office") {
    return (
      <div
        className={
          "office-preview office-preview--docx" +
          (embedded ? " office-preview--embedded" : "")
        }
      >
        {!embedded && (
          <div className="office-preview__bar">
            <Tip label={name}>
              <span className="office-preview__bar-title">
                {name}
              </span>
            </Tip>
            <div className="office-preview__bar-actions">
              <button
                type="button"
                className="btn btn--ghost btn--sm"
                onClick={() => void openExternal()}
              >
                {tr("office.openExternal")}
              </button>
            </div>
          </div>
        )}
        <div
          ref={docxScrollRef}
          className="office-preview__docx-scroll"
        >
          <div ref={docxRef} className="office-docx-host" />
        </div>
      </div>
    );
  }

  // XLSX — sheet tabs only when embedded; no filename title
  if (kind === "xlsx") {
    return (
      <div
        className={
          "office-preview office-preview--xlsx" +
          (embedded ? " office-preview--embedded" : "")
        }
      >
        {!embedded && (
          <div className="office-preview__bar">
            <Tip label={name}>
              <span className="office-preview__bar-title">
                {name}
              </span>
            </Tip>
            <div className="office-preview__bar-actions">
              <button
                type="button"
                className="btn btn--ghost btn--sm"
                onClick={() => void openExternal()}
              >
                {tr("office.openExternal")}
              </button>
            </div>
          </div>
        )}
        {sheetNames.length > 1 && (
          <div className="office-preview__sheets" role="tablist">
            {sheetNames.map((sn, i) => (
              <button
                key={sn}
                type="button"
                role="tab"
                className={
                  "office-preview__sheet-tab" +
                  (i === activeSheet ? " is-active" : "")
                }
                onClick={() => switchSheet(i)}
              >
                {sn}
              </button>
            ))}
          </div>
        )}
        <div
          className="office-preview__sheet-scroll"
          dangerouslySetInnerHTML={{ __html: DOMPurify.sanitize(sheetHtml) }}
        />
      </div>
    );
  }

  return (
    <div className="office-preview office-preview--center">
      <div className="office-preview__status">{tr("office.unsupported")}</div>
      <button type="button" className="btn btn--solid" onClick={() => void openExternal()}>
        {tr("office.openExternal")}
      </button>
    </div>
  );
}
