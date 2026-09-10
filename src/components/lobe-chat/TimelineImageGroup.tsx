import { useState } from "react";
import { createT, type Locale } from "@/i18n";
import { ImageUi, imageUiLabels } from "@/components/ImageUi";
import { IconPhoto, IconChevronDown, IconChevronRight } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import type { MessageToolSegment } from "@/lib/session";

/** 连续的图片读取共用一行；折叠时卸载预览并释放 Blob。 */
export function TimelineImageGroup({ tools, locale }: { tools: MessageToolSegment[]; locale: Locale }) {
  const [open, setOpen] = useState(true);
  const sources = tools.flatMap((tool) => tool.imageSources ?? []);
  const tr = createT(locale);
  return (
    <Collapsible open={open} onOpenChange={setOpen} className="lobe-timeline-tool" data-testid="timeline-images">
      <CollapsibleTrigger asChild>
        <Button className="lobe-timeline-tool__row">
          <span className="lobe-timeline-tool__icon" aria-hidden><IconPhoto size={17} /></span>
          <span className="lobe-timeline-tool__action">{tr(sources.length === 1 ? "tool.imageViewed" : "tool.imagesViewed", { count: sources.length })}</span>
          <span className="lobe-timeline-tool__primary" aria-hidden>{open ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}</span>
        </Button>
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="lobe-timeline-images">
          {sources.map((src, index) => (
            <ImageUi key={`${index}:${src}`} src={src} gallery={sources} layout="thumbnail"
              alt={tr("tool.imageNumber", { count: index + 1 })} labels={imageUiLabels(locale)} />
          ))}
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
}
