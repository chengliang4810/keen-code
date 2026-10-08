import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  Delete02Icon,
  Edit02Icon,
  Loading03Icon,
  PlugSocketIcon,
} from "@hugeicons/core-free-icons";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { effectiveCustomModel, type CustomModel } from "@/modules/ai/config";
import { useTranslation } from "@/modules/i18n";
import {
  formatModelContextWindow,
  reorderProviderModels,
} from "@/settings/lib/providerModelList";

const interactiveTarget =
  "button, input, textarea, select, a, [contenteditable=true], [data-no-model-drag]";

function ProviderModelRow({
  model,
  baseURL,
  disabled,
  onEdit,
  onRemove,
  onEnabledChange,
  onTest,
}: {
  model: CustomModel;
  baseURL: string;
  disabled: boolean;
  onEdit: () => void;
  onRemove: () => void;
  onEnabledChange: (enabled: boolean) => void;
  onTest: (signal: AbortSignal) => Promise<void>;
}) {
  const tr = useTranslation();
  const effective = effectiveCustomModel(model, baseURL);
  const contextLabel = tr("Context window: {value0}", {
    value0: effective.contextLimit.toLocaleString(),
  });
  const pending = useRef<AbortController | null>(null);
  const feedback = useRef<string | number | undefined>(undefined);
  const [testing, setTesting] = useState(false);
  useEffect(
    () => () => {
      pending.current?.abort();
      if (feedback.current !== undefined) toast.dismiss(feedback.current);
    },
    [],
  );
  const test = async () => {
    if (pending.current) return;
    const controller = new AbortController();
    pending.current = controller;
    setTesting(true);
    const id = toast.loading(
      tr("Testing model {value0}…", { value0: model.id }),
    );
    feedback.current = id;
    try {
      await onTest(controller.signal);
      if (!controller.signal.aborted)
        toast.success(tr("Model {value0} is available", { value0: model.id }), {
          id,
        });
    } catch {
      if (!controller.signal.aborted)
        toast.error(
          tr(
            "Model {value0} could not connect. Check the endpoint, API format and key.",
            { value0: model.id },
          ),
          { id },
        );
    } finally {
      if (pending.current === controller) {
        pending.current = null;
        if (!controller.signal.aborted) setTesting(false);
      }
    }
  };
  return (
    <div className="flex items-center gap-2 px-3 py-2">
      <div className="flex min-w-0 flex-1 items-center gap-2">
        <span
          className="min-w-0 truncate font-mono text-ui-base"
          title={model.id}
        >
          {model.id}
        </span>
        <span
          role="img"
          className="inline-flex h-5 max-w-20 shrink-0 items-center truncate rounded-md border border-border bg-background px-1.5 font-mono text-ui-sm text-muted-foreground"
          title={contextLabel}
          aria-label={contextLabel}
        >
          {formatModelContextWindow(effective.contextLimit)}
        </span>
        {effective.vision && (
          <span
            role="img"
            className="pointer-events-none inline-flex shrink-0 items-center rounded-full border border-border bg-background px-1 py-px text-ui-xs font-medium leading-normal text-muted-foreground"
            aria-label={tr("Vision")}
            title={tr("Vision")}
          >
            {tr("Vision")}
          </span>
        )}
      </div>
      <Button
        type="button"
        variant="ghost"
        size="icon-sm"
        className="shrink-0 p-0 text-muted-foreground"
        disabled={disabled || testing || !baseURL.trim()}
        aria-label={`${tr("Test model")} ${model.id}`}
        title={tr("Test model")}
        onClick={() => void test()}
      >
        <HugeiconsIcon
          icon={testing ? Loading03Icon : PlugSocketIcon}
          className={cn("size-3.5", testing && "animate-spin")}
        />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon-sm"
        className="shrink-0 p-0 text-muted-foreground"
        disabled={disabled}
        aria-label={`${tr("Edit model")} ${model.id}`}
        title={tr("Edit model")}
        onClick={onEdit}
      >
        <HugeiconsIcon icon={Edit02Icon} className="size-3.5" />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon-sm"
        className="shrink-0 text-muted-foreground"
        disabled={disabled}
        aria-label={`${tr("Remove model")} ${model.id}`}
        title={tr("Remove model")}
        onClick={onRemove}
      >
        <HugeiconsIcon icon={Delete02Icon} className="size-3.5" />
      </Button>
      <Switch
        size="sm"
        checked={model.enabled !== false}
        disabled={disabled}
        aria-label={`${tr(model.enabled === false ? "Enable model" : "Disable model")} ${model.id}`}
        onCheckedChange={onEnabledChange}
      />
    </div>
  );
}

export function ProviderModelList({
  models,
  baseURL,
  testScope,
  disabled,
  onChange,
  onEdit,
  onTest,
}: {
  models: readonly CustomModel[];
  baseURL: string;
  testScope: string;
  disabled: boolean;
  onChange: (models: CustomModel[]) => void;
  onEdit: (model: CustomModel) => void;
  onTest: (model: CustomModel, signal: AbortSignal) => Promise<void>;
}) {
  const tr = useTranslation();
  const list = useRef<HTMLUListElement>(null);
  const pointer = useRef<{
    id: string;
    pointerId: number;
    x: number;
    y: number;
    moved: boolean;
    over: string;
    top: number;
    height: number;
    node: HTMLLIElement;
  } | null>(null);
  const [dragging, setDragging] = useState<string | null>(null);
  const [over, setOver] = useState<string | null>(null);
  const keyboard = useRef<{ id: string; over: string; height: number } | null>(
    null,
  );
  const activeIndex = models.findIndex((model) => model.id === dragging);
  const overIndex = models.findIndex((model) => model.id === over);
  const rowHeight = pointer.current?.height ?? keyboard.current?.height ?? 0;
  const commitOrder = (active: string, target: string) => {
    const next = reorderProviderModels(models, active, target);
    if (next !== models && !disabled) onChange([...next]);
  };
  const cancel = useCallback(() => {
    if (pointer.current) pointer.current.node.style.transform = "";
    pointer.current = null;
    keyboard.current = null;
    setDragging(null);
    setOver(null);
  }, []);
  useEffect(() => {
    if (!dragging) return;
    const onEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      cancel();
    };
    window.addEventListener("keydown", onEscape, true);
    return () => window.removeEventListener("keydown", onEscape, true);
  }, [dragging, cancel]);
  return (
    <div className="overflow-x-auto overflow-y-hidden rounded-lg border border-border bg-background">
      <ul
        ref={list}
        aria-label={tr("Model list")}
        className="min-w-80"
        onFocusCapture={(event) => {
          if (dragging && (event.target as Element).closest(interactiveTarget))
            cancel();
        }}
      >
        {models.map((model, index) => (
          <li
            key={model.id}
            data-provider-model-id={model.id}
            className={cn(
              "min-w-0 touch-pan-y select-none border-border outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
              index < models.length - 1 && "border-b",
              !disabled && "cursor-grab active:cursor-grabbing",
              dragging === model.id &&
                "relative z-10 border-b border-transparent bg-card shadow-md",
            )}
            style={{
              transform:
                activeIndex < 0
                  ? undefined
                  : dragging === model.id
                    ? keyboard.current
                      ? `translateY(${(overIndex - activeIndex) * rowHeight}px)`
                      : undefined
                    : index > activeIndex && index <= overIndex
                      ? `translateY(${-rowHeight}px)`
                      : index < activeIndex && index >= overIndex
                        ? `translateY(${rowHeight}px)`
                        : undefined,
              transition:
                dragging && dragging !== model.id
                  ? "transform 150ms ease"
                  : undefined,
            }}
            tabIndex={disabled ? -1 : 0}
            aria-label={tr("Reorder model {value0}", { value0: model.id })}
            title={tr(
              "Drag to reorder. Press Space, then use arrows to move; Enter to confirm and Escape to cancel.",
            )}
            onPointerDown={(event) => {
              if (
                disabled ||
                event.button !== 0 ||
                !event.isPrimary ||
                (event.target as Element).closest(interactiveTarget)
              )
                return;
              pointer.current = {
                id: model.id,
                pointerId: event.pointerId,
                x: event.clientX,
                y: event.clientY,
                moved: false,
                over: model.id,
                top: list.current?.getBoundingClientRect().top ?? 0,
                height: event.currentTarget.getBoundingClientRect().height,
                node: event.currentTarget,
              };
              event.currentTarget.setPointerCapture(event.pointerId);
            }}
            onPointerMove={(event) => {
              const active = pointer.current;
              if (!active || active.pointerId !== event.pointerId) return;
              if (
                !active.moved &&
                Math.hypot(event.clientX - active.x, event.clientY - active.y) <
                  6
              )
                return;
              active.moved = true;
              active.node.style.transform = `translateY(${event.clientY - active.y}px)`;
              setDragging(active.id);
              const targetIndex = Math.max(
                0,
                Math.min(
                  models.length - 1,
                  Math.floor((event.clientY - active.top) / active.height),
                ),
              );
              active.over = models[targetIndex]?.id ?? active.id;
              setOver(active.over);
            }}
            onPointerUp={(event) => {
              const active = pointer.current;
              if (!active || active.pointerId !== event.pointerId) return;
              if (active.moved) commitOrder(active.id, active.over);
              cancel();
            }}
            onPointerCancel={cancel}
            onLostPointerCapture={() => {
              if (pointer.current) cancel();
            }}
            onKeyDown={(event) => {
              if (event.target !== event.currentTarget || disabled) return;
              if (event.key === "Escape") {
                event.preventDefault();
                cancel();
                return;
              }
              if (event.key === " " || event.key === "Enter") {
                event.preventDefault();
                if (keyboard.current) {
                  commitOrder(keyboard.current.id, keyboard.current.over);
                  cancel();
                } else {
                  keyboard.current = {
                    id: model.id,
                    over: model.id,
                    height: event.currentTarget.getBoundingClientRect().height,
                  };
                  setDragging(model.id);
                  setOver(model.id);
                }
              } else if (
                keyboard.current &&
                ["ArrowUp", "ArrowDown"].includes(event.key)
              ) {
                event.preventDefault();
                const target =
                  models.findIndex(
                    (item) => item.id === keyboard.current?.over,
                  ) + (event.key === "ArrowUp" ? -1 : 1);
                if (models[target]) {
                  keyboard.current.over = models[target].id;
                  setOver(models[target].id);
                }
              }
            }}
          >
            <ProviderModelRow
              key={`${testScope}:${model.id}`}
              model={model}
              baseURL={baseURL}
              disabled={disabled}
              onEdit={() => onEdit(model)}
              onRemove={() =>
                onChange(models.filter((item) => item.id !== model.id))
              }
              onEnabledChange={(enabled) =>
                onChange(
                  models.map((item) =>
                    item.id === model.id ? { ...item, enabled } : item,
                  ),
                )
              }
              onTest={(signal) => onTest(model, signal)}
            />
          </li>
        ))}
      </ul>
    </div>
  );
}
