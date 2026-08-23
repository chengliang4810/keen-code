import { useEffect, useMemo, useState } from "react";
import { IconChevronLeft, IconChevronRight, IconClose, IconRename } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import type { AskUserPayload, AskUserQuestionItem } from "@/lib/session";

export type AskUserLabels = {
  title: string; submit: string; next: string; cancel: string;
  otherPlaceholder: string; freeTextHint: string; multiHint: string; close: string;
};

type Props = {
  payload: AskUserPayload | null;
  labels: AskUserLabels;
  onSubmit: (answers: Record<string, string>) => void | Promise<void>;
  onCancel: () => void | Promise<void>;
};

export function buildAskUserAnswers(
  questions: AskUserQuestionItem[],
  selected: Record<string, string[]>,
  freeText: Record<string, string>,
): Record<string, string> {
  const answers: Record<string, string> = {};
  for (const question of questions) {
    const text = (freeText[question.id] || "").trim();
    if (text) {
      answers[question.id] = text;
      continue;
    }
    const optionIds = selected[question.id] || [];
    if (optionIds.length) {
      answers[question.id] = optionIds
        .map((id) => question.options.find((option) => option.id === id)?.label || id)
        .join(", ");
    }
  }
  return answers;
}

/** 当前会话内的提问卡片，不创建遮罩或窗口级弹层。 */
export function AskUserModal({ payload, labels, onSubmit, onCancel }: Props) {
  const questions = payload?.questions ?? [];
  const [page, setPage] = useState(0);
  const [selected, setSelected] = useState<Record<string, string[]>>({});
  const [freeText, setFreeText] = useState<Record<string, string>>({});
  const [editingText, setEditingText] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setPage(0); setSelected({}); setFreeText({}); setEditingText(false); setBusy(false);
  }, [payload?.rpcId]);

  const canSubmit = useMemo(() => questions.length > 0 && questions.every(
    (question) => Boolean((freeText[question.id] || "").trim()) ||
      (selected[question.id]?.length ?? 0) > 0,
  ), [questions, selected, freeText]);

  if (!payload || questions.length === 0) return null;
  const currentPage = Math.min(page, questions.length - 1);
  const question = questions[currentPage]!;
  const chosen = selected[question.id] || [];

  const submit = async () => {
    if (busy || !canSubmit) return;
    setBusy(true);
    try { await onSubmit(buildAskUserAnswers(questions, selected, freeText)); }
    finally { setBusy(false); }
  };
  const cancel = async () => {
    if (busy) return;
    setBusy(true);
    try { await onCancel(); } finally { setBusy(false); }
  };
  const choose = (optionId: string) => {
    setSelected((previous) => {
      const current = previous[question.id] || [];
      return { ...previous, [question.id]: question.multiSelect
        ? current.includes(optionId) ? current.filter((id) => id !== optionId) : [...current, optionId]
        : [optionId] };
    });
    setFreeText((previous) => ({ ...previous, [question.id]: "" }));
    setEditingText(false);
  };
  const goTo = (next: number) => {
    setPage(next); setEditingText(false);
  };

  return (
    <section className="ask-user" aria-label={labels.title}>
      <header className="ask-user__header">
        <h2 className="ask-user__prompt">{question.question}</h2>
        <div className="ask-user__nav">
          <Button type="button" className="ask-user__icon-btn" disabled={busy || currentPage === 0}
            aria-label="Previous question" onClick={() => goTo(currentPage - 1)}>
            <IconChevronLeft size={17} />
          </Button>
          <span className="ask-user__page" aria-live="polite">{currentPage + 1} / {questions.length}</span>
          <Button type="button" className="ask-user__icon-btn" disabled={busy || currentPage === questions.length - 1}
            aria-label={labels.next} onClick={() => goTo(currentPage + 1)}>
            <IconChevronRight size={17} />
          </Button>
          <Button type="button" className="ask-user__icon-btn" disabled={busy}
            aria-label={labels.close} onClick={() => void cancel()}>
            <IconClose size={18} />
          </Button>
        </div>
      </header>

      {question.multiSelect ? <p className="ask-user__hint">{labels.multiHint}</p> : null}
      <div className="ask-user__options" role="group" aria-label={question.question}>
        {question.options.map((option, index) => {
          const active = chosen.includes(option.id);
          return (
            <Button key={option.id} type="button"
              className={`ask-user__opt${active ? " ask-user__opt--active" : ""}`}
              disabled={busy} aria-pressed={active} onClick={() => choose(option.id)}>
              <span className="ask-user__index">{index + 1}</span>
              <span className="ask-user__opt-copy">
                <span className="ask-user__opt-label">{option.label}</span>
                {option.description ? <span className="ask-user__opt-desc">{option.description}</span> : null}
              </span>
              {active ? <IconChevronRight size={18} className="ask-user__opt-arrow" /> : null}
            </Button>
          );
        })}
      </div>

      {editingText || question.options.length === 0 ? (
        <Label className="ask-user__free">
          <span className="sr-only">{labels.otherPlaceholder}</span>
          <Textarea className="ask-user__textarea" rows={2} autoFocus
            value={freeText[question.id] || ""} disabled={busy}
            placeholder={labels.otherPlaceholder}
            onChange={(event) => {
              setFreeText((previous) => ({ ...previous, [question.id]: event.target.value }));
              setSelected((previous) => ({ ...previous, [question.id]: [] }));
            }} />
        </Label>
      ) : (
        <Button type="button" className="ask-user__custom" disabled={busy} onClick={() => setEditingText(true)}>
          <span className="ask-user__index"><IconRename size={15} /></span>
          <span>{labels.freeTextHint}</span>
        </Button>
      )}

      <footer className="ask-user__footer">
        <Button type="button" className="btn btn--ghost" disabled={busy} onClick={() => void cancel()}>{labels.cancel}</Button>
        <Button type="button" className="btn btn--solid" disabled={busy || !canSubmit} onClick={() => void submit()}>{labels.submit}</Button>
      </footer>
    </section>
  );
}
