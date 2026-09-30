import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";

/**
 * 右侧面板空状态启动器，对齐 ZCode 桌面 `AnimatedSidePanePanel` 的
 * `side-pane-open-tab-*` 结构：居中标题与说明，下方是可打开的标签列表；
 * 面板内联尺寸达到 480px 时列表由纵向排列切换为自适应网格（见 app-resource.css）。
 */

export interface SidePaneLauncherItem {
  id: string;
  label: string;
  icon: ReactNode;
  disabled?: boolean;
  onOpen: () => void;
}

export interface SidePaneLauncherProps {
  title: string;
  description: string;
  items: SidePaneLauncherItem[];
}

export function SidePaneLauncher({
  title,
  description,
  items,
}: SidePaneLauncherProps) {
  return (
    <div className="rp-launcher" data-testid="resource-launcher">
      <div className="rp-launcher__scroll">
        <div className="rp-launcher__content">
          <div className="rp-launcher__heading">
            <h2 className="rp-launcher__title">{title}</h2>
            <p className="rp-launcher__desc">{description}</p>
          </div>
          <div className="rp-launcher__list">
            {items.map((item) => (
              <Button
                key={item.id}
                type="button"
                variant="outline"
                size="md"
                className="rp-launcher__item"
                disabled={item.disabled}
                onClick={item.onOpen}
              >
                <span className="rp-launcher__item-icon" aria-hidden>
                  {item.icon}
                </span>
                <span className="rp-launcher__item-label">{item.label}</span>
              </Button>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
