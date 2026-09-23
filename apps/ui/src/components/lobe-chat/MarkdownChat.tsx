/**
 * Chat markdown — path/url → cards (image/video/file); open in resource pane.
 */

import {
  isValidElement,
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import type { Components } from "streamdown";
import type { PluggableList } from "unified";
import type { Locale } from "@/i18n";
import { createT } from "@/i18n";
import { ImageUi, imageUiLabels } from "@/components/ImageUi";
import { VideoUi, videoUiLabels } from "@/components/VideoUi";
import { FilePathCard } from "@/components/FilePathCard";
import type { ResourceOpenTarget } from "@/components/ResourceViewer";
import { HighlightedText } from "@/components/HighlightedText";
import { normalizeSingleDollarMath } from "@/lib/dollarMathGuard";
import { remarkAutolinkPunctuation } from "@/lib/remarkAutolinkPunctuation";
import {
  isImagePath,
  isVideoPath,
  resolveInlineMediaToken,
} from "@/lib/attachments";
import {
  classifyPathRef,
  fileSubtitle,
  isHttpUrl,
  looksLikeFilePath,
  normalizePathToken,
  resolveFileToken,
} from "@/lib/pathRefs";
import { isAbsoluteFsPath, pathBasename } from "@/lib/filePath";
import { reactNodeText } from "@/lib/reactNodeText";
import { cn } from "@/lib/utils";
import { CodeBlock } from "./CodeBlock";
import { StreamdownMarkdown } from "./StreamdownMarkdown";
import {
  findMarkdownTextBlock,
  selectMarkdownTextBlock,
} from "./markdownTextSelection";

const useCommittedLayoutEffect =
  typeof window === "undefined" ? useEffect : useLayoutEffect;

/** 稳定 identity,避免每个 token 重算 streamdown 的 remark 链。 */
const CHAT_REMARK_PLUGINS: PluggableList = [remarkAutolinkPunctuation];

/** 单个 img/code 块子节点解除段落包裹(对齐 streamdown 默认段落语义)。 */
function unwrapSingleBlockChild(c: ReactNode): ReactNode | null {
  const only = (Array.isArray(c) ? c : [c]).filter(
    (child) => child != null && child !== "",
  );
  if (only.length !== 1 || !isValidElement(only[0])) return null;
  const props = only[0].props as {
    node?: { tagName?: string };
    "data-block"?: unknown;
  };
  const tagName = props.node?.tagName;
  if (tagName === "img" || (tagName === "code" && "data-block" in props)) {
    return <>{c}</>;
  }
  return null;
}

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
          displayName={linkText && linkText !== rawIn ? linkText : undefined}
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
        displayName={linkText && linkText !== rawIn ? linkText : undefined}
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
    p: ({ children: c }) => unwrapSingleBlockChild(c) ?? <p>{paint(c)}</p>,
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
      const text = reactNodeText(c).trim();
      const hrefStr = typeof href === "string" ? href : "";
      if (isHttpUrl(hrefStr)) {
        return (
          <a
            className="chat-md__link"
            href={hrefStr}
            target="_blank"
            rel="noreferrer noopener"
            onClick={(event) => {
              if (!onOpenResourceRef.current) return;
              event.preventDefault();
              onOpenResourceRef.current({
                type: "url",
                url: hrefStr,
                title: text || undefined,
              });
            }}
          >
            {paint(c)}
          </a>
        );
      }
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
    code: (props) => {
      const { className: cnCode, children: c } = props;
      const match =
        typeof cnCode === "string"
          ? /language-([\w#+-]+)/.exec(cnCode)
          : null;
      // streamdown 的默认 pre 会给块级 code 克隆 `data-block` 属性;
      // 保留 className/换行嗅探作为兜底,不依赖单一标记。
      const block =
        "data-block" in props || Boolean(match) || String(c).includes("\n");
      if (!block) {
        return <code className="chat-md__inline-code">{paint(c)}</code>;
      }
      const lang = match?.[1] || "text";
      return (
        <CodeBlock
          language={lang}
          langLabel={lang === "text" ? tr("chat.codeLangText") : lang}
          wrapLabel={tr("chat.codeWrap")}
          unwrapLabel={tr("chat.codeUnwrap")}
          copyLabel={tr("message.copy")}
          previewLabel={tr("chat.codePreview")}
          previewTitle={tr("chat.codePreviewTitle")}
          previewDescription={tr("chat.codePreviewDescription")}
          previewLoading={tr("chat.codePreviewLoading")}
          previewCloseLabel={tr("common.close")}
          previewZoomInLabel={tr("chat.codePreviewZoomIn")}
          previewZoomOutLabel={tr("chat.codePreviewZoomOut")}
          previewResetLabel={tr("chat.codePreviewReset")}
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
    hr: () => <hr />,
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
  const plainComponents = useMemo(    () => buildComponents((node) => node),
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

  // `$` 公式启发式护栏只在源文本变化时重算;流式每个 delta 一次线性扫描。
  const guardedSource = useMemo(() => normalizeSingleDollarMath(source), [source]);

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
      <StreamdownMarkdown
        source={guardedSource}
        streaming={streaming}
        components={components}
        className="chat-md__doc"
        extraRemarkPlugins={CHAT_REMARK_PLUGINS}
        turnId={latencyTurnId}
      />
    </div>
  );
});
