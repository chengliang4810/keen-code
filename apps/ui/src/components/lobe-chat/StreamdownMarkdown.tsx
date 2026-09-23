/**
 * Streamdown 封装:聊天与资源面板共用的 markdown 渲染入口。
 *
 * 行为契约(移植自 ZCode message.tsx 的已验证模式,Apache-2.0):
 * - 只有真实流式走 `mode="streaming"` + `parseIncompleteMarkdown`;完成/历史
 *   内容固定 `static`,避免 Streamdown 内部分块状态在重挂载时反复同步
 *   (React 报错 185 一类更新风暴问题的第一道闸)。
 * - 单条 markdown 渲染异常时降级为 `whitespace-pre-wrap` 纯文本,错误不冒泡
 *   到会话区边界;内容变化(FNV hash)自动清除错误态重试。
 * - `recordMarkdownParse` 埋点以 remark 插件形式注入,保持旧观测口径。
 */

import {
  Component,
  memo,
  useMemo,
  type ReactNode,
} from "react";
import { Streamdown, type Components } from "streamdown";
import type { PluggableList } from "unified";
import { recordMarkdownParse } from "@/lib/frontendPerformance";
import { cn } from "@/lib/utils";
import {
  buildStreamdownRemarkPlugins,
  chatUrlTransform,
  streamdownControlsDisabled,
  streamdownLinkSafety,
  streamdownPlugins,
} from "./streamdownSetup";

const EMPTY_REMARK_PLUGINS: PluggableList = [];

/** FNV-1a,与内容等值比较相比更省(长消息每个 token 都要算)。 */
function hashMarkdownSource(markdown: string): string {
  let hash = 2166136261;
  for (let index = 0; index < markdown.length; index += 1) {
    hash ^= markdown.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `${markdown.length}:${hash >>> 0}`;
}

/**
 * 测量插件:attacher 记起点、transformer 结束时上报。streamdown 会按块复用
 * processor 缓存,因此口径是"该插件视角的解析时长",与旧
 * react-markdown 版 measureMarkdownParse 的相对趋势一致。
 */
function createMeasureRemarkPlugin(turnId: string, fullParse: boolean) {
  return function measurePlugin() {
    const started =
      typeof performance === "undefined" ? 0 : performance.now();
    return (
      _tree: unknown,
      file: { value?: unknown },
    ) => {
      const durationMs =
        typeof performance === "undefined"
          ? 0
          : performance.now() - started;
      recordMarkdownParse({
        turnId,
        durationMs,
        sourceChars: String(file?.value ?? "").length,
        fullParse,
      });
    };
  };
}

interface MarkdownBoundaryProps {
  children: ReactNode;
  className: string;
  fallbackText: string;
  resetKey: string;
  mode: "static" | "streaming";
}

interface MarkdownBoundaryState {
  error: Error | null;
}

function normalizeRenderError(error: unknown): Error {
  return error instanceof Error
    ? error
    : new Error(
        typeof error === "string" ? error : "Unknown markdown render error",
      );
}

class StreamdownMarkdownBoundary extends Component<
  MarkdownBoundaryProps,
  MarkdownBoundaryState
> {
  override state: MarkdownBoundaryState = { error: null };

  static getDerivedStateFromError(error: unknown): MarkdownBoundaryState {
    return { error: normalizeRenderError(error) };
  }

  override componentDidCatch(error: Error): void {
    console.warn("[StreamdownMarkdown] markdown 渲染失败,已降级为纯文本", {
      message: error.message,
      markdownLength: this.props.fallbackText.length,
      mode: this.props.mode,
    });
  }

  override componentDidUpdate(previousProps: MarkdownBoundaryProps): void {
    if (
      this.state.error !== null &&
      previousProps.resetKey !== this.props.resetKey
    ) {
      this.setState({ error: null });
    }
  }

  override render(): ReactNode {
    if (this.state.error) {
      return (
        <div
          className={cn(this.props.className, "whitespace-pre-wrap break-words")}
        >
          {this.props.fallbackText}
        </div>
      );
    }
    return this.props.children;
  }
}

export interface StreamdownMarkdownProps {
  /** 已完成预处理(`$` 护栏等)的 markdown 源文本。 */
  source: string;
  streaming: boolean;
  components: Components;
  /** 渲染根节点类名(即 streamdown 容器 div)。 */
  className?: string;
  /** 追加到默认 remark 链之后的插件(如链接标点修正)。 */
  extraRemarkPlugins?: PluggableList;
  /** 回合性能埋点;缺省时不记录。 */
  turnId?: string;
}

export const StreamdownMarkdown = memo(function StreamdownMarkdown({
  source,
  streaming,
  components,
  className,
  extraRemarkPlugins = EMPTY_REMARK_PLUGINS,
  turnId,
}: StreamdownMarkdownProps) {
  const remarkPlugins = useMemo(() => {
    const extra = [...extraRemarkPlugins];
    if (turnId) {
      extra.unshift(createMeasureRemarkPlugin(turnId, !streaming));
    }
    return buildStreamdownRemarkPlugins(extra);
  }, [extraRemarkPlugins, turnId, streaming]);

  const boundaryResetKey = streaming
    ? "streaming"
    : `static:${hashMarkdownSource(source)}`;

  return (
    <StreamdownMarkdownBoundary
      className={className ?? ""}
      fallbackText={source}
      resetKey={boundaryResetKey}
      mode={streaming ? "streaming" : "static"}
    >
      <Streamdown
        className={className}
        components={components}
        controls={streamdownControlsDisabled}
        linkSafety={streamdownLinkSafety}
        mode={streaming ? "streaming" : "static"}
        parseIncompleteMarkdown={streaming}
        plugins={streamdownPlugins}
        remarkPlugins={remarkPlugins}
        urlTransform={chatUrlTransform}
      >
        {source}
      </Streamdown>
    </StreamdownMarkdownBoundary>
  );
});
