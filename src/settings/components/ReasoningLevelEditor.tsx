import { useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useTranslation } from "@/modules/i18n";
import { validateReasoningLevels } from "@/modules/ai/lib/reasoning";

export function ReasoningLevelEditor({
  values,
  onChange,
}: {
  values: readonly string[];
  onChange: (values: string[]) => void;
}) {
  const tr = useTranslation();
  const [editing, setEditing] = useState<number | null>(null);
  const [text, setText] = useState("");
  const [error, setError] = useState("");
  const dragging = useRef<number | null>(null);
  const commit = () => {
    if (editing === null) return;
    const next = [...values];
    next[editing] = text.trim();
    try {
      validateReasoningLevels(next);
      if (next[editing] !== values[editing]) onChange(next);
      setEditing(null);
      setError("");
    } catch (e) {
      setError(tr((e as Error).message));
    }
  };
  const move = (from: number, to: number) => {
    if (to < 0 || to >= values.length || from === to) return;
    const next = [...values];
    const [value] = next.splice(from, 1);
    next.splice(to, 0, value);
    onChange(next);
  };
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-1.5">
        {values.map((level, index) => (
          <fieldset
            key={level}
            aria-label={level}
            className="group inline-flex h-8 items-center rounded-md border border-border"
            draggable={editing === null}
            onDragStart={() => {
              dragging.current = index;
            }}
            onDragOver={(event) => event.preventDefault()}
            onDrop={(event) => {
              event.preventDefault();
              if (dragging.current !== null) move(dragging.current, index);
              dragging.current = null;
            }}
            onDragEnd={() => {
              dragging.current = null;
            }}
          >
            {editing === index ? (
              <Input
                autoFocus
                aria-label={tr("Edit reasoning level")}
                className="h-7 w-24 border-0 font-mono"
                value={text}
                onChange={(event) => setText(event.target.value)}
                onBlur={commit}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    commit();
                  }
                  if (event.key === "Escape") {
                    event.preventDefault();
                    setEditing(null);
                    setError("");
                  }
                }}
              />
            ) : (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="h-7 cursor-grab px-2 font-mono text-ui-sm"
                title={tr("Drag or use Alt + arrows to reorder")}
                onClick={() => {
                  setEditing(index);
                  setText(level);
                  setError("");
                }}
                onKeyDown={(event) => {
                  if (
                    event.altKey &&
                    ["ArrowLeft", "ArrowRight"].includes(event.key)
                  ) {
                    event.preventDefault();
                    move(index, index + (event.key === "ArrowLeft" ? -1 : 1));
                  }
                }}
              >
                {level}
              </Button>
            )}
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              className="size-6 text-muted-foreground"
              aria-label={tr("Remove reasoning level {value0}", {
                value0: level,
              })}
              onClick={() => {
                setEditing(null);
                setError("");
                onChange(values.filter((_, i) => i !== index));
              }}
            >
              ×
            </Button>
          </fieldset>
        ))}
        {editing === values.length ? (
          <Input
            autoFocus
            aria-label={tr("New reasoning level")}
            className="h-8 w-28 font-mono"
            value={text}
            onChange={(event) => setText(event.target.value)}
            onBlur={() => {
              if (text.trim()) commit();
              else setEditing(null);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                commit();
              }
              if (event.key === "Escape") {
                event.preventDefault();
                setEditing(null);
                setError("");
              }
            }}
          />
        ) : (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="h-8"
            disabled={values.length >= 16}
            aria-label={tr("Add reasoning level")}
            onClick={() => {
              setEditing(values.length);
              setText("");
              setError("");
            }}
          >
            +
          </Button>
        )}
      </div>
      {error && (
        <p role="alert" className="text-ui-xs text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}
