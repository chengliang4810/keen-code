import { useEffect, useMemo, useRef, useState } from "react";
import { IconChevronLeft, IconChevronRight, IconClose, IconRename } from "@/components/icons";
import { Button } from "@appica/ui-react/button";
import { Textarea } from "@/components/ui/textarea";
import { Radio, RadioGroup } from "@/components/ui/radio-group";
import { Checkbox, CheckboxGroup } from "@/components/ui/checkbox-group";
import type { AskUserPayload, AskUserQuestionItem } from "@/lib/session";

export type AskUserLabels = {
  title: string; submit: string; next: string; cancel: string;
  otherPlaceholder: string; freeTextHint: string; multiHint: string; close: string;
};

type Props = {
  payload: AskUserPayload | null;
  labels: AskUserLabels;
  onSubmit: (answers: AskUserAnswers) => void | Promise<void>;
  onCancel: () => void | Promise<void>;
};

/** 单选和自由文本使用字符串，多选使用数组，避免用分隔符编码结构。 */
export type AskUserAnswers = Record<string, string | string[]>;

export function buildAskUserAnswers(
  questions: AskUserQuestionItem[],
  selected: Record<string, string[]>,
  freeText: Record<string, string>,
): AskUserAnswers {
  const answers: AskUserAnswers = {};
  for (const question of questions) {
    const text = (freeText[question.id] || "").trim();
    if (text) {
      answers[question.id] = question.multiSelect ? [text] : text;
      continue;
    }
    const optionIds = selected[question.id] || [];
    if (optionIds.length) {
      answers[question.id] = question.multiSelect ? optionIds : optionIds[0]!;
    }
  }
  return answers;
}

/** 当前会话内的提问卡片，不创建遮罩或窗口级弹层。 */
export function AskUserModal({ payload, labels, onSubmit, onCancel }: Props) {
  const questions = payload?.questions ?? [];
  const [page, setPage] = useState(0);
  const headingRef = useRef<HTMLHeadingElement>(null);
  useEffect(() => { headingRef.current?.focus(); }, [page]);
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

  const renderOptionRow = (option: AskUserQuestionItem["options"][number], index: number) => {
    const active = chosen.includes(option.id);
    return (
      <label key={option.id}
        className={
          `ask-user__opt${active ? " ask-user__opt--active" : ""}` +
          (busy ? " ask-user__opt--disabled" : "")
        }>
        {question.multiSelect
          ? <Checkbox value={option.id} disabled={busy} />
          : <Radio value={option.id} disabled={busy} />}
        <span className="ask-user__index">{index + 1}</span>
        <span className="ask-user__opt-copy">
          <span className="ask-user__opt-label">{option.label}</span>
          {option.description ? <span className="ask-user__opt-desc">{option.description}</span> : null}
        </span>
        {active ? <IconChevronRight size={18} className="ask-user__opt-arrow" /> : null}
      </label>
    );
  };

  /** 提交当前全部问题的结构化答案。 */
  const submit = async () => {
    if (busy || !canSubmit) return;
    setBusy(true);
    try { await onSubmit(buildAskUserAnswers(questions, selected, freeText)); }
    finally { setBusy(false); }
  };
  /** 取消当前问答并通知 Runtime。 */
  const cancel = async () => {
    if (busy) return;
    setBusy(true);
    try { await onCancel(); } finally { setBusy(false); }
  };
  /** 更新当前问题的选中项，并清除同题自由输入；单选答完自动进入下一题。 */
  const selectOption = (optionIds: string[]) => {
    setSelected((previous) => ({ ...previous, [question.id]: optionIds }));
    setFreeText((previous) => ({ ...previous, [question.id]: "" }));
    setEditingText(false);
    if (!question.multiSelect && optionIds.length > 0 && currentPage < questions.length - 1) {
      setPage(currentPage + 1);
    }
  };
  /** 切换到指定问题页并退出自由输入状态。 */
  const goTo = (next: number) => {
    setPage(next); setEditingText(false);
  };

  return (
    <section className="ask-user" aria-label={labels.title}>
      <header className="ask-user__header">
        <h2 ref={headingRef} tabIndex={-1} className="ask-user__prompt">{question.question}</h2>
        <div className="ask-user__nav">
          <Button type="button" variant="ghost" size="icon-md" disabled={busy || currentPage === 0}
            aria-label="Previous question" onClick={() => goTo(currentPage - 1)}>
            <IconChevronLeft size={17} />
          </Button>
          <span className="ask-user__page" aria-live="polite">{currentPage + 1} / {questions.length}</span>
          <Button type="button" variant="ghost" size="icon-md" disabled={busy || currentPage === questions.length - 1}
            aria-label={labels.next} onClick={() => goTo(currentPage + 1)}>
            <IconChevronRight size={17} />
          </Button>
          <Button type="button" variant="ghost" size="icon-md" disabled={busy}
            aria-label={labels.close} onClick={() => void cancel()}>
            <IconClose size={18} />
          </Button>
        </div>
      </header>

      {question.multiSelect ? <p className="ask-user__hint">{labels.multiHint}</p> : null}
      {question.multiSelect ? (
        <CheckboxGroup
          className="ask-user__options"
          aria-label={question.question}
          value={chosen}
          onValueChange={(value) => selectOption(value)}>
          {question.options.map(renderOptionRow)}
        </CheckboxGroup>
      ) : (
        <RadioGroup
          className="ask-user__options"
          aria-label={question.question}
          value={chosen[0] ?? ""}
          onValueChange={(value) => { if (typeof value === "string" && value) selectOption([value]); }}>
          {question.options.map(renderOptionRow)}
        </RadioGroup>
      )}

      {question.allowCustomAnswer !== false &&
      (editingText || question.options.length === 0) ? (
        <label className="ask-user__free">
          <span className="sr-only">{labels.otherPlaceholder}</span>
          <Textarea inputSize="md" className="ask-user__textarea" rows={2} autoFocus
            value={freeText[question.id] || ""} disabled={busy}
            placeholder={labels.otherPlaceholder}
            onChange={(event) => {
              setFreeText((previous) => ({ ...previous, [question.id]: event.target.value }));
              setSelected((previous) => ({ ...previous, [question.id]: [] }));
            }} />
        </label>
      ) : question.allowCustomAnswer !== false ? (
        <Button size="md" type="button" variant="ghost" className="ask-user__custom" disabled={busy} onClick={() => setEditingText(true)}>
          <span className="ask-user__index"><IconRename size={15} /></span>
          <span>{labels.freeTextHint}</span>
        </Button>
      ) : null}

      <footer className="ask-user__footer">
        <Button size="md" type="button" variant="ghost" disabled={busy} onClick={() => void cancel()}>{labels.cancel}</Button>
        <Button size="md" type="button" variant="primary" disabled={busy || !canSubmit} onClick={() => void submit()}>{labels.submit}</Button>
      </footer>
    </section>
  );
}
