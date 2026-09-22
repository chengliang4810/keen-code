import Lightbox from "yet-another-react-lightbox";
import Zoom from "yet-another-react-lightbox/plugins/zoom";
import "yet-another-react-lightbox/styles.css";

export interface ImageLightboxSlide {
  src: string;
  alt?: string;
  title?: string;
}

interface ImageLightboxProps {
  open: boolean;
  index: number;
  slides: ImageLightboxSlide[];
  onClose: () => void;
  onView: (index: number) => void;
  onExited: () => void;
  labels: {
    next: string;
    previous: string;
    close: string;
    zoomIn: string;
    zoomOut: string;
  };
}

export default function ImageLightbox({
  open,
  index,
  slides,
  onClose,
  onView,
  onExited,
  labels,
}: ImageLightboxProps) {
  return (
    <Lightbox
      open={open}
      close={onClose}
      index={index}
      slides={slides}
      on={{ view: ({ index: next }) => onView(next), exited: onExited }}
      plugins={[Zoom]}
      zoom={{ maxZoomPixelRatio: 4, scrollToZoom: true }}
      carousel={{ finite: slides.length <= 1, preload: 2 }}
      controller={{ closeOnBackdropClick: true }}
      styles={{ container: { backgroundColor: "rgba(0, 0, 0, 0.92)" } }}
      labels={{
        Next: labels.next,
        Previous: labels.previous,
        Close: labels.close,
        "Zoom in": labels.zoomIn,
        "Zoom out": labels.zoomOut,
      }}
    />
  );
}
