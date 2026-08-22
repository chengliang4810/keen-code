import { Button } from "@/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import type { Locale } from "@/i18n";
import type {
  AcpArtifactReference,
  AcpStructuredToolResult,
  AcpToolResultItem,
} from "@/lib/acp/types";
import {
  IconAlertTriangle,
  IconExternalLink,
  IconFileDiff,
  IconFileText,
} from "@/components/icons";

/** 结构化工具结果视图属性。 */
export interface StructuredToolResultViewProps {
  /** 当前界面语言。 */
  locale: Locale;
  /** 工具稳定名称。 */
  toolName?: string;
  /** Agent 返回的权威结构化工具结果。 */
  result: AcpStructuredToolResult;
  /** 打开本地文件或 Diff 的工作台回调。 */
  onOpenPath?: (path: string) => void;
}

/** 结构化工具结果使用的本地化文案。 */
interface StructuredResultLabels {
  /** 结果标题。 */
  result: string;
  /** 截断提示。 */
  truncated: string;
  /** 原始大小标签。 */
  originalSize: string;
  /** 标准输出标签。 */
  stdout: string;
  /** 标准错误标签。 */
  stderr: string;
  /** 退出码标签。 */
  exitCode: string;
  /** 耗时标签。 */
  duration: string;
  /** 打开文件命令。 */
  open: string;
  /** 产物标签。 */
  artifact: string;
  /** 扩展标签。 */
  extensions: string;
}

/** 返回结构化工具结果的中英文文案。 */
function labelsForLocale(locale: Locale): StructuredResultLabels {
  if (locale === "zh") {
    return {
      result: "结构化结果",
      truncated: "输出已截断，完整内容保留在产物中",
      originalSize: "原始大小",
      stdout: "标准输出",
      stderr: "标准错误",
      exitCode: "退出码",
      duration: "耗时",
      open: "打开",
      artifact: "产物",
      extensions: "扩展",
    };
  }
  return {
    result: "Structured result",
    truncated: "Output was truncated; the full content is retained as an artifact",
    originalSize: "Original size",
    stdout: "stdout",
    stderr: "stderr",
    exitCode: "Exit code",
    duration: "Duration",
    open: "Open",
    artifact: "Artifact",
    extensions: "Extensions",
  };
}

/** 将字节数格式化为紧凑的二进制单位。 */
function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

/** 将毫秒格式化为适合工具行展示的耗时。 */
function formatDuration(durationMs: number): string {
  return durationMs < 1000
    ? `${durationMs} ms`
    : `${(durationMs / 1000).toFixed(2)} s`;
}

/** 渲染一个可按需打开的本地产物引用。 */
function ArtifactRow({
  artifact,
  labels,
  onOpenPath,
}: {
  /** Agent 返回的本地产物引用。 */
  artifact: AcpArtifactReference;
  /** 当前文案集合。 */
  labels: StructuredResultLabels;
  /** 打开本地路径的回调。 */
  onOpenPath?: (path: string) => void;
}) {
  return (
    <div className="structured-result__artifact">
      <div>
        <strong>{labels.artifact}</strong>
        <code title={artifact.path ?? artifact.id}>
          {artifact.path ?? artifact.id}
        </code>
        <span>
          {artifact.media_type} · {formatBytes(artifact.size_bytes)}
        </span>
      </div>
      {artifact.path && onOpenPath ? (
        <Button
          type="button"
          className="chrome-btn"
          title={labels.open}
          aria-label={`${labels.open}: ${artifact.path}`}
          onClick={() => onOpenPath(artifact.path!)}
        >
          <IconExternalLink size={13} />
        </Button>
      ) : null}
    </div>
  );
}

/** 渲染结构化工具结果中的一个类型化条目。 */
function ResultItem({
  item,
  labels,
  onOpenPath,
}: {
  /** 需要渲染的稳定结果条目。 */
  item: AcpToolResultItem;
  /** 当前文案集合。 */
  labels: StructuredResultLabels;
  /** 打开本地路径的回调。 */
  onOpenPath?: (path: string) => void;
}) {
  switch (item.type) {
    case "text":
      return <pre className="structured-result__text">{item.text}</pre>;
    case "diff":
      return (
        <div className="structured-result__item">
          <div className="structured-result__item-head">
            <span>
              <IconFileDiff size={13} />
              <code title={item.path}>{item.path}</code>
            </span>
            {onOpenPath ? (
              <Button
                type="button"
                className="chrome-btn"
                title={labels.open}
                aria-label={`${labels.open}: ${item.path}`}
                onClick={() => onOpenPath(item.path)}
              >
                <IconExternalLink size={13} />
              </Button>
            ) : null}
          </div>
          {item.old_path && item.old_path !== item.path ? (
            <div className="structured-result__rename">
              {item.old_path} → {item.path}
            </div>
          ) : null}
          <pre className="structured-result__diff">{item.patch}</pre>
        </div>
      );
    case "file":
      return (
        <div className="structured-result__item-head">
          <span>
            <IconFileText size={13} />
            <code title={item.path}>{item.path}</code>
            <small>{item.operation}</small>
            {typeof item.size_bytes === "number" ? (
              <small>{formatBytes(item.size_bytes)}</small>
            ) : null}
          </span>
          {onOpenPath ? (
            <Button
              type="button"
              className="chrome-btn"
              title={labels.open}
              aria-label={`${labels.open}: ${item.path}`}
              onClick={() => onOpenPath(item.path)}
            >
              <IconExternalLink size={13} />
            </Button>
          ) : null}
        </div>
      );
    case "command":
      return (
        <div className="structured-result__command">
          <div className="structured-result__command-head">
            <code>{item.command}</code>
            <span>
              {labels.exitCode}: {item.exit_code ?? "signal"}
              {typeof item.duration_ms === "number"
                ? ` · ${labels.duration}: ${formatDuration(item.duration_ms)}`
                : ""}
            </span>
          </div>
          {item.stdout ? (
            <Collapsible className="structured-result__command-details">
              <CollapsibleTrigger className="structured-result__command-trigger">
                {labels.stdout}
              </CollapsibleTrigger>
              <CollapsibleContent>
                <pre>{item.stdout}</pre>
              </CollapsibleContent>
            </Collapsible>
          ) : null}
          {item.stderr ? (
            <Collapsible
              className="structured-result__command-details"
              defaultOpen={item.exit_code !== 0}
            >
              <CollapsibleTrigger className="structured-result__command-trigger">
                {labels.stderr}
              </CollapsibleTrigger>
              <CollapsibleContent>
                <pre className="is-error">{item.stderr}</pre>
              </CollapsibleContent>
            </Collapsible>
          ) : null}
        </div>
      );
    case "image":
      return (
        <figure className="structured-result__image">
          <img
            src={`data:${item.media_type};base64,${item.data}`}
            alt={item.label ?? ""}
            loading="lazy"
          />
          {item.label ? <figcaption>{item.label}</figcaption> : null}
        </figure>
      );
    case "artifact":
      return (
        <ArtifactRow
          artifact={item.artifact}
          labels={labels}
          onOpenPath={onOpenPath}
        />
      );
  }
}

/** 按类型渲染 Diff、文件、命令、图片和产物等稳定工具结果。 */
export function StructuredToolResultView({
  locale,
  toolName,
  result,
  onOpenPath,
}: StructuredToolResultViewProps) {
  const labels = labelsForLocale(locale);
  const items = result.items ?? [];
  return (
    <section
      className={
        "structured-result" + (result.is_error ? " is-error" : "")
      }
      aria-label={`${toolName ? `${toolName} · ` : ""}${labels.result}`}
    >
      <header className="structured-result__head">
        <span>{labels.result}</span>
        {toolName ? <code>{toolName}</code> : null}
      </header>
      {result.truncated ? (
        <div className="structured-result__warning" role="status">
          <IconAlertTriangle size={13} />
          <span>
            {labels.truncated}
            {typeof result.original_bytes === "number"
              ? ` · ${labels.originalSize}: ${formatBytes(result.original_bytes)}`
              : ""}
          </span>
        </div>
      ) : null}
      {items.length > 0 ? (
        <div className="structured-result__items">
          {items.map((item, index) => (
            <ResultItem
              key={`${item.type}:${index}`}
              item={item}
              labels={labels}
              onOpenPath={onOpenPath}
            />
          ))}
        </div>
      ) : result.output ? (
        <pre className="structured-result__text">{result.output}</pre>
      ) : null}
      {result.artifact ? (
        <ArtifactRow
          artifact={result.artifact}
          labels={labels}
          onOpenPath={onOpenPath}
        />
      ) : null}
      {result.extensions?.length ? (
        <div className="structured-result__extensions">
          <span>{labels.extensions}</span>
          {result.extensions.map((extension) => (
            <code key={String(extension.namespace)}>
              {String(extension.namespace)}
            </code>
          ))}
        </div>
      ) : null}
    </section>
  );
}
