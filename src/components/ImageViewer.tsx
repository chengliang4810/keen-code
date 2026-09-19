/**
 * Global image lightbox (yet-another-react-lightbox) + open/copy helpers.
 * Zoom and prev/next; right-click on the active slide copies the image.
 */

import {
  createContext,
  lazy,
  Suspense,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import {
  releaseImageSrc,
  resolveImageSrcs,
} from "@/lib/imageSrc";
import { copyImageFromPath, copyImageFromSrc } from "@/lib/copyImage";
import { createT, type Locale } from "@/i18n";

const ImageLightbox = lazy(() => import("@/components/ImageLightbox"));

export interface ImageSlideInput {
  /** Local absolute path or already-viewable URL. */
  src: string;
  alt?: string;
  title?: string;
}

export interface ImageViewerApi {
  /** Open lightbox with slides (paths or URLs). Resolves local paths async. */
  open: (slides: ImageSlideInput[] | string[], index?: number) => void;
  close: () => void;
  /** Copy image at path/URL to clipboard. Returns true on success. */
  copyImage: (pathOrUrl: string) => Promise<boolean>;
}

const ImageViewerContext = createContext<ImageViewerApi | null>(null);

/** Safe hook when provider may be absent (returns no-ops). */
export function useImageViewerOptional(): ImageViewerApi {
  const ctx = useContext(ImageViewerContext);
  return (
    ctx ?? {
      open: () => {},
      close: () => {},
      copyImage: async () => false,
    }
  );
}

interface ResolvedSlide {
  src: string;
  alt?: string;
  title?: string;
  /** Original path/url for copy. */
  origin: string;
}

interface ImageViewerProviderProps {
  children: ReactNode;
  locale: Locale;
}

export function ImageViewerProvider({
  children,
  locale,
}: ImageViewerProviderProps) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [isOpen, setIsOpen] = useState(false);
  const [index, setIndex] = useState(0);
  const [slides, setSlides] = useState<ResolvedSlide[]>([]);

  useEffect(
    () => () => slides.forEach((slide) => releaseImageSrc(slide.src)),
    [slides],
  );

  const close = useCallback(() => {
    setIsOpen(false);
  }, []);

  const openViewer = useCallback(
    (input: ImageSlideInput[] | string[], startIndex = 0) => {
      const normalized: ImageSlideInput[] = input.map((item) =>
        typeof item === "string" ? { src: item } : item,
      );
      if (!normalized.length) return;

      void (async () => {
        const paths = normalized.map((s) => s.src);
        const resolved = await resolveImageSrcs(paths);
        if (!resolved.length) return;

        const meta = new Map(normalized.map((s) => [s.src, s] as const));
        const next: ResolvedSlide[] = resolved.map(({ path, src }) => {
          const m = meta.get(path);
          return {
            src,
            origin: path,
            alt: m?.alt ?? m?.title,
            title: m?.title,
          };
        });

        const want =
          normalized[Math.min(startIndex, normalized.length - 1)]?.src;
        let idx = next.findIndex((s) => s.origin === want);
        if (idx < 0) idx = 0;

        setSlides(next);
        setIndex(idx);
        setIsOpen(true);
      })();
    },
    [],
  );

  /** 本地路径解析要走 IPC，交给 copyImageFromPath 放进手势内的写入载荷。 */
  const copyImage = useCallback(async (pathOrUrl: string) => {
    return (await copyImageFromPath(pathOrUrl)).ok;
  }, []);

  const api = useMemo<ImageViewerApi>(
    () => ({
      open: openViewer,
      close,
      copyImage,
    }),
    [openViewer, close, copyImage],
  );

  // Right-click inside lightbox → copy current image (keeps Zoom plugin intact).
  useEffect(() => {
    if (!isOpen) return;
    const onCtx = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      if (!target?.closest?.(".yarl__root")) return;
      const img = target.closest("img") as HTMLImageElement | null;
      if (!img) return;
      const src = img.currentSrc || img.src;
      if (!src) return;
      e.preventDefault();
      e.stopPropagation();
      void copyImageFromSrc(src);
    };
    document.addEventListener("contextmenu", onCtx, true);
    return () => document.removeEventListener("contextmenu", onCtx, true);
  }, [isOpen]);

  return (
    <ImageViewerContext.Provider value={api}>
      {children}
      {slides.length > 0 ? (
        <Suspense fallback={null}>
          <ImageLightbox
            open={isOpen}
            onClose={close}
            index={index}
            slides={slides.map((s) => ({
              src: s.src,
              alt: s.alt ?? s.title,
              title: s.title,
            }))}
            onView={setIndex}
            onExited={() => setSlides([])}
            labels={{
              next: tr("image.next"),
              previous: tr("image.prev"),
              close: tr("image.close"),
              zoomIn: tr("image.zoomIn"),
              zoomOut: tr("image.zoomOut"),
            }}
          />
        </Suspense>
      ) : null}
    </ImageViewerContext.Provider>
  );
}
