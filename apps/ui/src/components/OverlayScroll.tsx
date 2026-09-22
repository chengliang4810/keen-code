import { ScrollArea } from "@appica/ui-react/scroll-area";
import type { CSSProperties, ReactNode, Ref, UIEvent } from "react";

type OverlayScrollProps = {
  children: ReactNode;
  className?: string;
  /** Extra class on the scrolling viewport (keeps layout classes like messages). */
  viewportClassName?: string;
  style?: CSSProperties;
  /** Forward scroll events */
  onScroll?: (e: UIEvent<HTMLDivElement>) => void;
  /** Optional external ref to the scrolling viewport element. */
  viewportRef?: Ref<HTMLDivElement | null>;
};

export function OverlayScroll({
  children,
  className = "",
  viewportClassName = "",
  style,
  onScroll,
  viewportRef: viewportRefProp,
}: OverlayScrollProps) {
  return (
    <ScrollArea
      className={"overlay-scroll" + (className ? ` ${className}` : "")}
      style={style}
      scrollbarVisibility="auto"
      viewportProps={{
        ref: viewportRefProp,
        className:
          "overlay-scroll__viewport" +
          (viewportClassName ? ` ${viewportClassName}` : ""),
        onScroll,
      }}
    >
      {children}
    </ScrollArea>
  );
}
