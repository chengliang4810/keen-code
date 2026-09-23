/**
 * Assistant message body — GFM markdown with safe defaults.
 * Images open the global lightbox; videos play inline; right-click menus.
 * Path links/code become media cards when imagePathMap is set.
 */

import { useMemo } from "react";
import type { Components } from "streamdown";
import type { PluggableList } from "unified";
import { remarkAutolinkPunctuation } from "@/lib/remarkAutolinkPunctuation";
import { normalizeSingleDollarMath } from "@/lib/dollarMathGuard";
import { StreamdownMarkdown } from "@/components/lobe-chat/StreamdownMarkdown";
import type { Locale } from "@/i18n";
import { ImageUi, imageUiLabels } from "@/components/ImageUi";
import { VideoUi, videoUiLabels } from "@/components/VideoUi";
import {
  isImagePath,
  isVideoPath,
  resolveInlineMediaToken,
  resolveMediaHref,
} from "@/lib/attachments";
import { isAbsoluteFsPath, pathBasename } from "@/lib/filePath";
import { reactNodeText } from "@/lib/reactNodeText";

const RESOURCE_REMARK_PLUGINS: PluggableList = [remarkAutolinkPunctuation];

export function MarkdownBody({
  children,
  streaming,
  locale = "en",
  imagePathMap,
}: {
  children: string;
  streaming?: boolean;
  locale?: Locale;
  imagePathMap?: Record<string, string>;
}) {
  const imageLabels = useMemo(() => imageUiLabels(locale), [locale]);
  const videoLabels = useMemo(() => videoUiLabels(locale), [locale]);
  const gallery = useMemo(() => {
    if (!imagePathMap) return undefined;
    return Array.from(new Set(Object.values(imagePathMap))).filter(isImagePath);
  }, [imagePathMap]);

  const renderMedia = (abs: string, alt?: string) => {
    if (isVideoPath(abs)) {
      return (
        <VideoUi
          key={abs}
          src={abs}
          path={abs}
          title={alt || pathBasename(abs)}
          labels={videoLabels}
        />
      );
    }
    return (
      <ImageUi
        className="md-body__img md-body__img--card"
        src={abs}
        alt={alt || pathBasename(abs)}
        path={abs}
        gallery={gallery}
        labels={imageLabels}
      />
    );
  };

  const components = useMemo<Components>(
    () => ({
      a: ({ href, children: c }) => {
        const text = reactNodeText(c).trim();
        const abs = resolveMediaHref(href, text, imagePathMap);
        if (abs) return renderMedia(abs, text || pathBasename(abs));
        return (
          <a href={href} target="_blank" rel="noreferrer noopener">
            {c}
          </a>
        );
      },
      pre: ({ children: c }) => <pre className="md-body__pre">{c}</pre>,
      code: ({ className, children: c }) => {
        const inline = !className;
        if (inline) {
          return <code className="md-body__code-inline">{c}</code>;
        }
        return <code className={className}>{c}</code>;
      },
      img: ({ src, alt }) => {
        if (!src) return null;
        const mapped =
          resolveInlineMediaToken(src, imagePathMap) ?? src;
        if (isVideoPath(mapped)) {
          return renderMedia(
            mapped,
            typeof alt === "string" ? alt : pathBasename(mapped),
          );
        }
        const local = isAbsoluteFsPath(mapped) ? mapped : undefined;
        return (
          <ImageUi
            className="md-body__img md-body__img--card"
            src={mapped}
            alt={alt ?? ""}
            path={local}
            gallery={gallery}
            labels={imageLabels}
          />
        );
      },
      table: ({ children: c }) => (
        <div className="md-body__table-wrap">
          <table>{c}</table>
        </div>
      ),
    }),
    // renderMedia 只消费下面这几个稳定值;imagePathMap 保持引用语义。
    [gallery, imageLabels, imagePathMap, videoLabels],
  );

  return (
    <div
      className={
        "md-body" + (streaming ? " md-body--streaming" : "")
      }
    >
      <StreamdownMarkdown
        source={normalizeSingleDollarMath(children || (streaming ? " " : ""))}
        streaming={!!streaming}
        components={components}
        className="md-body__doc"
        extraRemarkPlugins={RESOURCE_REMARK_PLUGINS}
      />
    </div>
  );
}
