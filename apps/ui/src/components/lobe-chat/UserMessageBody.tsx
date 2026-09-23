import {
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import { Button } from "@appica/ui-react/button";
import { IconChevronDown } from "@/components/icons";

export const USER_MESSAGE_COLLAPSED_HEIGHT = 120;

/** 内容超过折叠阈值时才需要显示展开入口。 */
export function isUserMessageOverflowing(
  scrollHeight: number,
  collapsedHeight = USER_MESSAGE_COLLAPSED_HEIGHT,
): boolean {
  return scrollHeight > collapsedHeight + 1;
}

/** 展开入口只负责在两个稳定状态之间切换，便于交互和无障碍测试复用。 */
export function toggleUserMessageExpanded(expanded: boolean): boolean {
  return !expanded;
}

/** 新消息内容不能继承上一条消息的展开状态。 */
export function resetUserMessageExpanded(
  expanded: boolean,
  previousContentKey: string,
  contentKey: string,
): boolean {
  return previousContentKey === contentKey ? expanded : false;
}

/** ZCode 风格的长用户消息：默认折叠到 120px，并在内容实际溢出时提供切换入口。 */
export function UserMessageBody({
  children,
  contentKey,
  expandLabel,
  collapseLabel,
}: {
  children: ReactNode;
  contentKey: string;
  expandLabel: string;
  collapseLabel: string;
}) {
  const contentRef = useRef<HTMLDivElement | null>(null);
  const frameRef = useRef<number | null>(null);
  const previousContentKeyRef = useRef(contentKey);
  const [contentHeight, setContentHeight] = useState(
    USER_MESSAGE_COLLAPSED_HEIGHT,
  );
  const [expandable, setExpandable] = useState(false);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    const previousContentKey = previousContentKeyRef.current;
    previousContentKeyRef.current = contentKey;
    setExpanded((current) =>
      resetUserMessageExpanded(current, previousContentKey, contentKey),
    );
  }, [contentKey]);

  useEffect(() => {
    const content = contentRef.current;
    if (!content) return;
      const update = () => {
        const nextHeight = content.scrollHeight;
        setContentHeight(nextHeight);
        setExpandable(isUserMessageOverflowing(nextHeight));
    };
    const schedule = () => {
      if (frameRef.current !== null) return;
      frameRef.current = requestAnimationFrame(() => {
        frameRef.current = null;
        update();
      });
    };
    schedule();
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    observer?.observe(content);
    if (!observer) window.addEventListener("resize", schedule);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", schedule);
      if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    };
  }, [contentKey]);

  const label = expanded ? collapseLabel : expandLabel;
  return (
    <div className="lobe-chat-user-body">
      <div
        ref={contentRef}
        className={
          "lobe-chat-user-body__content" +
          (!expanded && expandable ? " is-collapsed" : "") +
          (expanded ? " is-expanded" : "")
        }
        style={{
          "--user-message-content-height": `${Math.max(contentHeight, USER_MESSAGE_COLLAPSED_HEIGHT)}px`,
        } as CSSProperties}
      >
        {children}
      </div>
      {expandable ? (
        <div className={"lobe-chat-user-body__toggle" + (expanded ? " is-expanded" : "")}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            aria-label={label}
            title={label}
            aria-expanded={expanded}
            onClick={() => setExpanded(toggleUserMessageExpanded)}
          >
            <IconChevronDown size={14} className={expanded ? "is-expanded" : undefined} />
          </Button>
        </div>
      ) : null}
    </div>
  );
}
