import { memo, useCallback, useEffect, useState, type ReactNode } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useStickToBottomContext } from "use-stick-to-bottom";
import type { ConversationTurn } from "@/modules/ai/lib/conversationPresentation";

type Props = {
  turns: readonly ConversationTurn[];
  renderTurn: (turn: ConversationTurn) => ReactNode;
};

export const ConversationTimeline = memo(function ConversationTimeline({
  turns,
  renderTurn,
}: Props) {
  const history = turns.slice(0, -1);
  const tail = turns[turns.length - 1];
  return (
    <div data-conversation-timeline className="min-w-0 flex-1">
      {history.length > 24 ? (
        <VirtualHistory turns={history} renderTurn={renderTurn} />
      ) : (
        history.map((turn) => <div key={turn.id}>{renderTurn(turn)}</div>)
      )}
      {/* 活跃轮始终挂载，历史窗口变化不会重置流式 Markdown 或审批交互。 */}
      {tail && (
        <div key={tail.id} data-conversation-tail>
          {renderTurn(tail)}
        </div>
      )}
    </div>
  );
});

const VirtualHistory = memo(
  function VirtualHistory({ turns, renderTurn }: Props) {
    const { scrollRef } = useStickToBottomContext();
    const [scrollElement, setScrollElement] = useState<HTMLElement | null>(
      null,
    );
    // 外层滚动节点的 ref 在子组件首次布局后才就绪，提交后再启用观察，避免空历史窗口。
    useEffect(() => {
      setScrollElement(scrollRef.current);
    }, [scrollRef]);
    const getScrollElement = useCallback(() => scrollElement, [scrollElement]);
    const getItemKey = useCallback((index: number) => turns[index].id, [turns]);
    const virtualizer = useVirtualizer({
      count: turns.length,
      getScrollElement,
      getItemKey,
      estimateSize: () => 300,
      overscan: 3,
    });
    return (
      <div
        data-conversation-history="virtual"
        className="relative w-full"
        style={{ height: virtualizer.getTotalSize() }}
      >
        {virtualizer.getVirtualItems().map((item) => (
          <div
            key={item.key}
            data-index={item.index}
            ref={virtualizer.measureElement}
            className="absolute left-0 top-0 w-full"
            style={{ transform: `translateY(${item.start}px)` }}
          >
            {renderTurn(turns[item.index])}
          </div>
        ))}
      </div>
    );
  },
  (previous, next) =>
    previous.renderTurn === next.renderTurn &&
    previous.turns.length === next.turns.length &&
    previous.turns.every((turn, index) => turn === next.turns[index]),
);
