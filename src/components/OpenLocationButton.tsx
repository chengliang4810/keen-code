/**
 * “打开位置”分段按钮：主按钮执行当前目标，展开按钮切换系统打开方式。
 */

import { useCallback, useRef, useState } from "react";
import { createPortal } from "react-dom";
import * as api from "@/lib/api";
import { useFloatingMenu } from "@/lib/floatingMenu";
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
  const rootRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  const { pos, style } = useFloatingMenu({
    open,
    triggerRef: rootRef,
    panelRef,
    onClose: () => setOpen(false),
    placement: "down",
    fitContent: true,
    estHeight: 340,
    gap: 6,
  });

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

  const menu =
    open && pos && typeof document !== "undefined"
      ? createPortal(
          <div
            ref={panelRef}
            className="menu-panel open-loc-menu"
            role="menu"
            style={style}
          >
            <button
              type="button"
              role="menuitem"
              className={
                "open-loc-menu__item" +
                (active === "finder" || active === "explorer"
                  ? " is-active"
                  : "")
              }
              onClick={() => {
                setOpen(false);
                void openWith(finderTarget, true);
              }}
            >
              <span className="open-loc-menu__ico" aria-hidden>
                <IconFolder size={16} />
              </span>
              <span>{labels.finder}</span>
            </button>
            <button
              type="button"
              role="menuitem"
              className={
                "open-loc-menu__item" +
                (active === "system" ? " is-active" : "")
              }
              onClick={() => {
                setOpen(false);
                void openWith("system", true);
              }}
            >
              <span className="open-loc-menu__ico" aria-hidden>
                <IconExternalLink size={16} />
              </span>
              <span>{labels.systemDefault}</span>
            </button>
            <div className="open-loc-menu__sep" aria-hidden />
            <button
              type="button"
              role="menuitem"
              className="open-loc-menu__item"
              onClick={() => {
                setOpen(false);
                if (!path) return;
                void navigator.clipboard
                  .writeText(path)
                  .then(() => onCopied?.())
                  .catch((e) => onOpenError?.(String(e)));
              }}
            >
              <span className="open-loc-menu__ico" aria-hidden>
                <IconCopy size={16} />
              </span>
              <span>{labels.copyPath}</span>
            </button>
          </div>,
          document.body,
        )
      : null;

  return (
    <div
      ref={rootRef}
      className={
        "open-loc" +
        (open ? " is-open" : "") +
        (compact ? " open-loc--compact" : "") +
        (disabled ? " is-disabled" : "") +
        (className ? ` ${className}` : "")
      }
    >
      <Tip label={labels.openHint} disabled={disabled}>
        <button
          type="button"
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
        </button>
      </Tip>
      <Tip label={labels.openMenu} disabled={disabled}>
        <button
          type="button"
          className="open-loc__caret"
          disabled={disabled}
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={() => setOpen((value) => !value)}
        >
          <IconChevronDown size={12} className="chevron" />
        </button>
      </Tip>
      {menu}
    </div>
  );
}
