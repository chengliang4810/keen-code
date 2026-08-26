/**
 * Chat markdown — path/url → cards (image/video/file); open in resource pane.
 */

import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import type { Components } from "react-markdown";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import { ImageUi, imageUiLabels } from "@/components/ImageUi";
import { VideoUi, videoUiLabels } from "@/components/VideoUi";
import { FilePathCard } from "@/components/FilePathCard";
import type { ResourceOpenTarget } from "@/components/ResourceViewer";
import { HighlightedText } from "@/components/HighlightedText";
import {
  isImagePath,
  isVideoPath,
  pathBasename,
  resolveInlineMediaToken,
} from "@/lib/attachments";
import {
  classifyPathRef,
  fileSubtitle,
  isAbsoluteFsPath,
  isHttpUrl,
  looksLikeFilePath,
  normalizePathToken,
  resolveFileToken,
} from "@/lib/pathRefs";
import { cn } from "@/lib/utils";
import { CodeBlock } from "./CodeBlock";
import { IncrementalMarkdown } from "./IncrementalMarkdown";
import {
  findMarkdownTextBlock,
  selectMarkdownTextBlock,
} from "./markdownTextSelection";

const useCommittedLayoutEffect =
  typeof window === "undefined" ? useEffect : useLayoutEffect;

/** Highlight string leaves for in-chat find (markdown-safe). */
function highlightChildren(
  children: ReactNode,
  query: string,
  activeOccurrence: number | null | undefined,
  counter: { n: number },
): ReactNode {
  const q = query.trim();
  if (!q) return children;
  if (typeof children === "string" || typeof children === "number") {
    const text = String(children);
    const base = counter.n;
    // Count matches in this leaf so subsequent leaves get correct indices.
    const lower = text.toLowerCase();
    const qLower = q.toLowerCase();
    let from = 0;
    let local = 0;
    while (from < lower.length) {
      const at = lower.indexOf(qLower, from);
      if (at < 0) break;
      local += 1;
      from = at + q.length;
    }
    const activeLocal =
      activeOccurrence != null &&
      activeOccurrence >= base &&
      activeOccurrence < base + local
        ? activeOccurrence - base
        : null;
    counter.n += local;
    if (local === 0) return text;
    return (
      <HighlightedText
        text={text}
        query={q}
        activeOccurrence={activeLocal}
      />
    );
  }
  if (Array.isArray(children)) {
    return children.map((c, i) => (
      <span key={i}>
        {highlightChildren(c, query, activeOccurrence, counter)}
      </span>
    ));
  }
  return children;
}

function textFromChildren(children: ReactNode): string {
  if (children == null || children === false) return "";
  if (typeof children === "string" || typeof children === "number") {
    return String(children);
  }
  if (Array.isArray(children)) {
    return children.map(textFromChildren).join("");
  }
  return "";
}

function handleMarkdownMouseDown(event: ReactMouseEvent<HTMLDivElement>) {
  if (
    event.button !== 0 ||
    event.detail !== 3 ||
    !(event.target instanceof Element)
  ) {
    return;
  }
  const block = findMarkdownTextBlock(event.target);
  if (
    block &&
    event.currentTarget.contains(block) &&
    selectMarkdownTextBlock(block, event.detail)
  ) {
    event.preventDefault();
    event.stopPropagation();
  }
}

export const MarkdownChat = memo(function MarkdownChat({
  children,
  streaming = false,
  locale = "en",
  className,
  muted,
  imagePathMap,
  projectPath,
  onOpenResource,
  findQuery = "",
  findActiveOccurrence = null,
  findOccurrenceBase = 0,
  onFirstVisibleToken,
  latencyTurnId,
}: {
  children: string;
  streaming?: boolean;
  locale?: Locale;
  className?: string;
  muted?: boolean;
  imagePathMap?: Record<string, string>;
  projectPath?: string | null;
  onOpenResource?: (target: ResourceOpenTarget) => void;
  /** In-chat find query — highlights string leaves in markdown. */
  findQuery?: string;
  findActiveOccurrence?: number | null;
  /** Starting occurrence index for multi-segment assistant bodies. */
  findOccurrenceBase?: number;
  /** Markdown 文本实际提交到 DOM 后记录首个可见 Token。 */
  onFirstVisibleToken?: (turnId: string) => void;
  /** 已完成回合用于排除迟到的旧 DOM effect；流式阶段可为空。 */
  latencyTurnId?: string;
}) {
  const tr = useMemo(() => createT(locale), [locale]);
  const imageLabels = useMemo(() => imageUiLabels(locale), [locale]);
  const videoLabels = useMemo(() => videoUiLabels(locale), [locale]);
  const fileLabels = useMemo(
    () => ({
      open: tr("attach.open"),
      reveal: tr("attach.reveal"),
      copyPath: tr("attach.copyPath"),
      openInPanel: tr("resources.openInPanel"),
      openExternal: tr("resources.openExternal"),
      details: tr("attach.details"),
      detailsTitle: tr("attach.detailsTitle"),
      detailsName: tr("attach.detailsName"),
      detailsType: tr("attach.detailsType"),
      detailsPath: tr("attach.detailsPath"),
      detailsResolved: tr("attach.detailsResolved"),
      detailsStatus: tr("attach.detailsStatus"),
      detailsOk: tr("attach.detailsOk"),
      detailsClose: tr("attach.detailsClose"),
      typeFile: tr("attach.typeFile"),
      typeUrl: tr("attach.typeUrl"),
      typeDir: tr("attach.typeDir"),
    }),
    [tr],
  );
  const gallery = useMemo(() => {
    if (!imagePathMap) return undefined;
    return Array.from(new Set(Object.values(imagePathMap))).filter(isImagePath);
  }, [imagePathMap]);
  // App-level resource callbacks are currently inline. A ref keeps Markdown
  // component identities stable across token renders without baking in a
  // stale click handler inside frozen prefix blocks.
  const onOpenResourceRef = useRef(onOpenResource);
  onOpenResourceRef.current = onOpenResource;

  // App 的 ACP 状态投影是唯一 rAF 合并层；这里直接解析最新已发布文本，
  // 避免第二个动画帧再次推迟 reasoning/正文。
  const source = children || "";
  const firstVisibleCallbackRef = useRef(onFirstVisibleToken);
  firstVisibleCallbackRef.current = onFirstVisibleToken;
  const reportedVisibleKeyRef = useRef<string | null>(null);
  const visibleKey = latencyTurnId ?? "live";
  useCommittedLayoutEffect(() => {
    if (
      !latencyTurnId ||
      !source.trim() ||
      reportedVisibleKeyRef.current === visibleKey
    ) {
      return;
    }
    reportedVisibleKeyRef.current = visibleKey;
    firstVisibleCallbackRef.current?.(latencyTurnId);
  }, [latencyTurnId, source, visibleKey]);

  const renderPathOrUrl = useCallback((token: string, linkText?: string) => {
    const rawIn = token.trim().replace(/^<|>$/g, "");
    if (!rawIn) return null;
    // Prefer ellipsis-stripped form for open/search; keep original for display map
    const raw = normalizePathToken(rawIn) || rawIn;

    if (isHttpUrl(rawIn) || isHttpUrl(raw)) {
      const url = isHttpUrl(rawIn) ? rawIn : raw;
      return (
        <FilePathCard
          path={url}
          kind="url"
          projectPath={projectPath}
          labels={fileLabels}
          onOpenInPanel={(t) => {
            if (t.type === "url" && t.url) {
              onOpenResourceRef.current?.({
                type: "url",
                url: t.url,
                title: t.title,
              });
            }
          }}
        />
      );
    }

    const mediaAbs =
      resolveInlineMediaToken(raw, imagePathMap) ||
      resolveInlineMediaToken(rawIn, imagePathMap);
    if (mediaAbs && isImagePath(mediaAbs)) {
      return (
        <ImageUi
          className="md-body__img md-body__img--card"
          src={mediaAbs}
          alt={linkText || pathBasename(mediaAbs)}
          path={mediaAbs}
          gallery={gallery}
          labels={imageLabels}
        />
      );
    }
    if (mediaAbs && isVideoPath(mediaAbs)) {
      return (
        <VideoUi
          key={mediaAbs}
          src={mediaAbs}
          path={mediaAbs}
          title={linkText || pathBasename(mediaAbs)}
          labels={videoLabels}
        />
      );
    }

    if (!looksLikeFilePath(rawIn) && !looksLikeFilePath(raw) && !mediaAbs) {
      return null;
    }

    // No naive projectRoot+relative join — FilePathCard uses host smart open.
    const resolved =
      mediaAbs ||
      resolveFileToken(raw, { projectPath, pathMap: imagePathMap }) ||
      resolveFileToken(rawIn, { projectPath, pathMap: imagePathMap });
    if (
      !resolved &&
      !looksLikeFilePath(raw) &&
      !looksLikeFilePath(rawIn)
    ) {
      return null;
    }

    // Prefer multi-segment relative after ellipsis strip for smart open
    const pathToken = resolved || raw || rawIn;
    const kind = classifyPathRef(pathToken);
    // Only inline media when we already have an absolute path; relative
    // tokens go through FilePathCard → host smart open (sibling KB / suffix).
    if (
      kind === "image" &&
      resolved &&
      isAbsoluteFsPath(resolved) &&
      isImagePath(resolved)
    ) {
      return (
        <ImageUi
          className="md-body__img md-body__img--card"
          src={resolved}
          alt={linkText || pathBasename(resolved)}
          path={resolved}
          gallery={gallery}
          labels={imageLabels}
        />
      );
    }
    if (
      kind === "video" &&
      resolved &&
      isAbsoluteFsPath(resolved) &&
      isVideoPath(resolved)
    ) {
      return (
        <VideoUi
          key={resolved}
          src={resolved}
          path={resolved}
          title={linkText || pathBasename(resolved)}
          labels={videoLabels}
        />
      );
    }

    return (
      <FilePathCard
        path={pathToken}
        absolutePath={
          resolved && isAbsoluteFsPath(resolved) ? resolved : undefined
        }
        projectPath={projectPath}
        kind="file"
        subtitle={fileSubtitle(pathToken, locale === "en" ? "en" : "zh")}
        labels={fileLabels}
        onOpenInPanel={(t) => {
          if (t.type === "file" && t.path) {
            onOpenResourceRef.current?.({
              type: "file",
              path: t.path,
              title: t.title,
            });
          }
        }}
      />
    );
  }, [
    fileLabels,
    gallery,
    imageLabels,
    imagePathMap,
    locale,
    projectPath,
    videoLabels,
  ]);

  const qFind = findQuery.trim();
  const buildComponents = (
    paint: (node: ReactNode) => ReactNode,
  ): Components => ({
    p: ({ children: c }) => <p>{paint(c)}</p>,
    li: ({ children: c }) => <li>{paint(c)}</li>,
    strong: ({ children: c }) => <strong>{paint(c)}</strong>,
    em: ({ children: c }) => <em>{paint(c)}</em>,
    h1: ({ children: c }) => <h1>{paint(c)}</h1>,
    h2: ({ children: c }) => <h2>{paint(c)}</h2>,
    h3: ({ children: c }) => <h3>{paint(c)}</h3>,
    h4: ({ children: c }) => <h4>{paint(c)}</h4>,
    blockquote: ({ children: c }) => <blockquote>{paint(c)}</blockquote>,
    td: ({ children: c }) => <td>{paint(c)}</td>,
    th: ({ children: c }) => <th>{paint(c)}</th>,
    a: ({ href, children: c }) => {
      const text = textFromChildren(c).trim();
      const hrefStr = typeof href === "string" ? href : "";
      const card =
        (hrefStr && renderPathOrUrl(hrefStr, text)) ||
        (text && text !== hrefStr ? renderPathOrUrl(text) : null);
      if (card) return card;
      return (
        <a
          className="chat-md__link"
          href={href}
          target="_blank"
          rel="noreferrer noopener"
        >
          {paint(c)}
        </a>
      );
    },
    pre: ({ children: c }) => <>{c}</>,
    code: ({ className: cnCode, children: c }) => {
      const match =
        typeof cnCode === "string"
          ? /language-([\w#+-]+)/.exec(cnCode)
          : null;
      const block = Boolean(match) || String(c).includes("\n");
      if (!block) {
        return <code className="chat-md__inline-code">{paint(c)}</code>;
      }
      return (
        <CodeBlock
          language={match?.[1] || "text"}
          wrapLabel={tr("chat.codeWrap")}
          unwrapLabel={tr("chat.codeUnwrap")}
          copyLabel={tr("message.copy")}
          highlight={!streaming}
        >
          {c as ReactNode}
        </CodeBlock>
      );
    },
    table: ({ children: c }) => (
      <div className="chat-md__table-wrap">
        <table>{c}</table>
      </div>
    ),
    hr: () => null,
    img: ({ src, alt }) => {
      if (!src || typeof src !== "string") return null;
      const card = renderPathOrUrl(
        src,
        typeof alt === "string" ? alt : undefined,
      );
      if (card) return card;
      return (
        <ImageUi
          className="md-body__img md-body__img--card"
          src={src}
          alt={typeof alt === "string" ? alt : ""}
          labels={imageLabels}
        />
      );
    },
  });

  // Stable during token updates, so memoized prefix segments do not re-render.
  const plainComponents = useMemo(
    () => buildComponents((node) => node),
    [imageLabels, renderPathOrUrl, streaming, tr],
  );
  // Find is an interactive exceptional path: use one full document parse so
  // occurrence indices remain global across all Markdown blocks.
  const findCounter = { n: findOccurrenceBase };
  const components = qFind
    ? buildComponents((node) =>
        highlightChildren(node, qFind, findActiveOccurrence, findCounter),
      )
    : plainComponents;

  return (
    <div
      className={cn(
        "chat-md",
        muted && "chat-md--muted",
        streaming && "chat-md--streaming",
        className,
      )}
      onMouseDown={handleMarkdownMouseDown}
    >
      <IncrementalMarkdown
        source={source}
        streaming={streaming}
        components={components}
        disabled={!!qFind}
      />
    </div>
  );
});
