import { useMemo, useState, type ReactNode } from "react";
import { Popover, PopoverContent, PopoverTrigger } from "@appica/ui-react/popover";
import { Button } from "@/components/ui/button";
import { SearchField } from "@/components/SearchField";
import { IconChevronDown, IconClose } from "@/components/icons";

/**
 * 标签总览弹层，对齐 ZCode 桌面 `SidePaneTabOverview`：在标签条最左侧提供一个
 * 下拉入口，列出当前已打开的标签并支持搜索与直接关闭。弹层只读取调用方传入的
 * 标签快照，关闭动作仍由 ResourceViewer 的既有 close* 回调承担。
 */

export interface SidePaneTabOverviewItem {
  key: string;
  label: string;
  title: string;
  icon: ReactNode;
}

export interface SidePaneTabOverviewProps {
  items: readonly SidePaneTabOverviewItem[];
  activeKey: string;
  labels: {
    trigger: string;
    search: string;
    group: string;
    noResults: string;
    closeTab: string;
  };
  onActivate: (key: string) => void;
  onClose: (key: string) => void;
}

/** 大小写不敏感的子串匹配；空查询返回全部标签。 */
function matchesQuery(label: string, title: string, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return (
    label.toLowerCase().includes(q) || title.toLowerCase().includes(q)
  );
}

export function SidePaneTabOverview({
  items,
  activeKey,
  labels,
  onActivate,
  onClose,
}: SidePaneTabOverviewProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const filtered = useMemo(
    () => items.filter((item) => matchesQuery(item.label, item.title, query)),
    [items, query],
  );

  return (
    <Popover
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (!next) setQuery("");
      }}
    >
      <PopoverTrigger
        render={
          <Button
            type="button"
            variant="ghost"
            size="icon-md"
            className="rp-tabs__overview-trigger"
            aria-label={labels.trigger}
          />
        }
      >
        <IconChevronDown size={15} />
      </PopoverTrigger>
      <PopoverContent
        align="start"
        arrow={false}
        className="rp-tabs__overview"
        aria-label={labels.trigger}
      >
        <SearchField
          containerClassName="rp-tabs__overview-search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={labels.search}
          aria-label={labels.search}
          iconSize={13}
        />
        <div className="rp-tabs__overview-group" role="group" aria-label={labels.group}>
          {filtered.length === 0 ? (
            <div className="rp-tabs__overview-empty">{labels.noResults}</div>
          ) : (
            filtered.map((item) => (
              <div
                key={item.key}
                className={
                  "rp-tabs__overview-row" +
                  (item.key === activeKey ? " is-active" : "")
                }
              >
                <button
                  type="button"
                  data-design-system-allow
                  className="rp-tabs__overview-item"
                  title={item.title}
                  onClick={() => {
                    onActivate(item.key);
                    setOpen(false);
                  }}
                >
                  <span className="rp-tabs__overview-icon" aria-hidden>
                    {item.icon}
                  </span>
                  <span className="rp-tabs__overview-label">{item.label}</span>
                </button>
                <TipClose
                  label={labels.closeTab}
                  onClick={() => onClose(item.key)}
                />
              </div>
            ))
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}

function TipClose({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <Button
      type="button"
      variant="ghost"
      size="icon-md"
      className="rp-tabs__overview-close"
      aria-label={label}
      title={label}
      onClick={onClick}
    >
      <IconClose size={12} />
    </Button>
  );
}
