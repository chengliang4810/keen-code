import { Button } from "@/components/ui/button";
import { ButtonGroup } from "@appica/ui-react/button-group";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@appica/ui-react/dropdown-menu";
/**
 * “打开位置”分段按钮：主按钮执行当前目标，展开按钮切换系统打开方式。
 */

import { useCallback, useState } from "react";
import * as api from "@/lib/api";
import {
  IconChevronDown,
  IconCopy,
  IconExternalLink,
  IconFolder,
} from "@/components/icons";
import { Tip } from "@/components/ui/tooltip";

export type OpenLocationTarget = "finder" | "explorer" | "system";

export interface OpenLocationButtonProps {
  /** Absolute path to open (project root or file). Hidden when null/empty. */
  path: string | null | undefined;
  /** Last selected target id (persisted by parent). */
  target: OpenLocationTarget;
  /** Called when user picks a menu item (parent should persist). */
  onTargetChange: (target: OpenLocationTarget) => void;
  /** Optional: after open success / always after attempt. */
  onOpenError?: (err: string) => void;
  /** Optional toast/feedback after path is copied. */
  onCopied?: () => void;
  platform?: "mac" | "win" | "linux" | "other";
  labels: {
    openLocation: string;
    openHint: string;
    openMenu: string;
    finder: string;
    systemDefault: string;
    /** Last menu item — copy absolute path. */
    copyPath: string;
  };
  className?: string;
  /** Compact: icon + caret only (no label). */
  compact?: boolean;
  disabled?: boolean;
}

/** 把外部传入值限制为当前支持的唯一目标集。 */
function normalizeTarget(
  target: OpenLocationTarget,
  platform: "mac" | "win" | "linux" | "other",
): OpenLocationTarget {
  if (target === "system") return target;
  if (platform === "win" && target === "explorer") return target;
  if (platform !== "win" && target === "finder") return target;
  return platform === "win" ? "explorer" : "finder";
}

/** 渲染文件夹定位、系统打开与路径复制操作。 */
export function OpenLocationButton({
  path,
  target,
  onTargetChange,
  onOpenError,
  onCopied,
  platform = "mac",
  labels,
  className = "",
  compact = false,
  disabled = false,
}: OpenLocationButtonProps) {
  const [open, setOpen] = useState(false);

  const active = normalizeTarget(target, platform);

  const openWith = useCallback(
    async (raw: OpenLocationTarget, remember: boolean) => {
      if (!path || disabled) return;
      const t = normalizeTarget(raw, platform);
      if (remember) onTargetChange(t);
      try {
        if (t === "finder" || t === "explorer") {
          await api.pathReveal(path);
        } else {
          await api.pathOpen(path);
        }
      } catch (e) {
        onOpenError?.(String(e));
      }
    },
    [path, disabled, onTargetChange, onOpenError, platform],
  );

  if (!path) return null;

  const finderTarget = platform === "win" ? "explorer" : "finder";

  return (
    <ButtonGroup
      variant="outline"
      size="md"
      disabled={disabled}
      className={
        "open-loc" +
        (open ? " is-open" : "") +
        (compact ? " open-loc--compact" : "") +
        (className ? ` ${className}` : "")
      }
    >
      <Tip label={labels.openHint} disabled={disabled}>
      <Button
        type="button"
        variant="outline"
        className="open-loc__main"
          disabled={disabled}
          onClick={() => void openWith(active, false)}
        >
          <span className="open-loc__app-ico" aria-hidden>
            {active === "system" ? (
              <IconExternalLink size={15} />
            ) : (
              <IconFolder size={15} />
            )}
          </span>
          {!compact && (
            <span className="open-loc__label">{labels.openLocation}</span>
          )}
        </Button>
      </Tip>
      <DropdownMenu open={open} onOpenChange={setOpen} size="md">
        <Tip label={labels.openMenu} disabled={disabled}>
          <DropdownMenuTrigger
            className="open-loc__caret"
            disabled={disabled}
            render={<Button type="button" variant="outline" size="icon-md" />}
          >
            <IconChevronDown size={12} className="chevron" />
          </DropdownMenuTrigger>
        </Tip>
        <DropdownMenuContent align="end" sideOffset={6}>
          <DropdownMenuRadioGroup
            value={active}
            onValueChange={(value) => {
              void openWith(value as OpenLocationTarget, true);
            }}
          >
            <DropdownMenuRadioItem value={finderTarget}>
              <span data-icon="start" aria-hidden>
                <IconFolder size={16} />
              </span>
              <span>{labels.finder}</span>
            </DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="system">
              <span data-icon="start" aria-hidden>
                <IconExternalLink size={16} />
              </span>
              <span>{labels.systemDefault}</span>
            </DropdownMenuRadioItem>
          </DropdownMenuRadioGroup>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            onClick={() => {
              void navigator.clipboard
                .writeText(path)
                .then(() => onCopied?.())
                .catch((error) => onOpenError?.(String(error)));
            }}
          >
            <span data-icon="start" aria-hidden>
              <IconCopy size={16} />
            </span>
            <span>{labels.copyPath}</span>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </ButtonGroup>
  );
}
