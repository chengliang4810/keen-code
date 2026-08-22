import { Button } from "@/components/ui/button";
/** 侧栏底部固定操作：设置入口以及按需显示的更新入口。 */

import { IconDownload, IconSettings } from "@/components/icons";

export interface UserMenuProps {
  labels: {
    settings: string;
    update: string;
  };
  updateAvailable: boolean;
  updateBusy: boolean;
  onSettings: () => void;
  onUpdate: () => void;
}

/** 渲染无需弹层的侧栏底部操作。 */
export function UserMenu({
  labels,
  updateAvailable,
  updateBusy,
  onSettings,
  onUpdate,
}: UserMenuProps) {
  return (
    <div className="user-menu user-menu--inline">
      <div className="user-menu__actions">
        <Button
          type="button"
          className="sidebar-footer-action"
          onClick={onSettings}
          title={labels.settings}
          aria-label={labels.settings}
        >
          <IconSettings size={16} />
          <span>{labels.settings}</span>
        </Button>
        {updateAvailable ? (
          <Button
            type="button"
            className="sidebar-update-action"
            onClick={onUpdate}
            disabled={updateBusy}
            title={labels.update}
            aria-label={labels.update}
            aria-busy={updateBusy || undefined}
          >
            <IconDownload size={17} />
          </Button>
        ) : null}
      </div>
    </div>
  );
}
