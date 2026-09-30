import { Button } from "@/components/ui/button";
/** 侧栏底部固定操作：设置、按需显示的更新入口以及手机远控。 */

import {
  IconDeviceMobile,
  IconDownload,
  IconSettings,
} from "@/components/icons";
import { Tip } from "@/components/ui/tooltip";

export interface UserMenuProps {
  labels: {
    settings: string;
    update: string;
  };
  remoteControl?: {
    label: string;
    onClick: () => void;
  };
  updateAvailable: boolean;
  updateBusy: boolean;
  onSettings: () => void;
  onUpdate: () => void;
}

/** 渲染无需弹层的侧栏底部操作。 */
export function UserMenu({
  labels,
  remoteControl,
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
          variant="ghost"
          size="md"
          className="sidebar-footer-action"
          onClick={onSettings}
          aria-label={labels.settings}
        >
          <IconSettings size={16} />
          <span>{labels.settings}</span>
        </Button>
        {updateAvailable ? (
          <Tip label={labels.update}>
            <Button
              type="button"
              variant="ghost"
              size="icon-md"
              className="sidebar-update-action"
              onClick={onUpdate}
              disabled={updateBusy}
              aria-label={labels.update}
              aria-busy={updateBusy || undefined}
            >
              <IconDownload size={17} />
            </Button>
          </Tip>
        ) : null}
        {remoteControl ? (
          <Tip label={remoteControl.label}>
            <Button
              type="button"
              variant="ghost"
              size="icon-md"
              className="sidebar-remote-action"
              onClick={remoteControl.onClick}
              aria-label={remoteControl.label}
            >
              <IconDeviceMobile size={17} />
            </Button>
          </Tip>
        ) : null}
      </div>
    </div>
  );
}
