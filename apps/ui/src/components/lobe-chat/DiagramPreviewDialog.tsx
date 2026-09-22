import { useEffect, useMemo, useState, type CSSProperties } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogBody,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Tip } from "@/components/ui/tooltip";
import {
  IconMaximize,
  IconMinimize,
  IconPlus,
  IconRefresh,
} from "@/components/icons";

export interface DiagramPreviewLabels {
  title: string;
  description: string;
  close: string;
  zoomIn: string;
  zoomOut: string;
  reset: string;
}

const MIN_ZOOM = 0.5;
const MAX_ZOOM = 3;
const ZOOM_STEP = 0.25;

export function DiagramPreviewDialog({
  open,
  svg,
  labels,
  onOpenChange,
}: {
  open: boolean;
  svg: string | null;
  labels: DiagramPreviewLabels;
  onOpenChange: (open: boolean) => void;
}) {
  const [zoom, setZoom] = useState(1);

  useEffect(() => {
    if (open) setZoom(1);
  }, [open, svg]);

  const zoomLabel = useMemo(() => `${Math.round(zoom * 100)}%`, [zoom]);
  if (!svg) return null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        frame={false}
        closeButton
        closeLabel={labels.close}
        className="chat-diagram-dialog"
      >
        <DialogHeader className="chat-diagram-dialog__header">
          <DialogTitle>{labels.title}</DialogTitle>
          <DialogDescription className="sr-only">
            {labels.description}
          </DialogDescription>
        </DialogHeader>
        <DialogBody className="chat-diagram-dialog__body">
          <div className="chat-diagram-dialog__viewport">
            <div
              className="chat-diagram-dialog__canvas"
              style={{ "--chat-diagram-scale": zoom } as CSSProperties}
              dangerouslySetInnerHTML={{ __html: svg }}
            />
          </div>
          <div className="chat-diagram-dialog__controls" aria-label={labels.title}>
            <Tip label={labels.zoomOut}>
              <Button
                type="button"
                variant="ghost"
                size="icon-md"
                aria-label={labels.zoomOut}
                disabled={zoom <= MIN_ZOOM}
                onClick={() => setZoom((value) => Math.max(MIN_ZOOM, value - ZOOM_STEP))}
              >
                <IconMinimize size={16} />
              </Button>
            </Tip>
            <span className="chat-diagram-dialog__zoom" aria-live="polite">
              {zoomLabel}
            </span>
            <Tip label={labels.zoomIn}>
              <Button
                type="button"
                variant="ghost"
                size="icon-md"
                aria-label={labels.zoomIn}
                disabled={zoom >= MAX_ZOOM}
                onClick={() => setZoom((value) => Math.min(MAX_ZOOM, value + ZOOM_STEP))}
              >
                <IconPlus size={16} />
              </Button>
            </Tip>
            <Tip label={labels.reset}>
              <Button
                type="button"
                variant="ghost"
                size="icon-md"
                aria-label={labels.reset}
                onClick={() => setZoom(1)}
              >
                <IconRefresh size={16} />
              </Button>
            </Tip>
            <IconMaximize size={15} className="chat-diagram-dialog__hint" />
          </div>
        </DialogBody>
      </DialogContent>
    </Dialog>
  );
}
