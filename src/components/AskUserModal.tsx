import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { Button } from "@/components/ui/button";
/** Agent 提问弹窗；使用 GlassModal 承载当前 ACP 问题结构。 */

import { useEffect, useMemo, useState } from "react";
import { GlassModal } from "@/components/GlassModal";
import type { AskUserPayload, AskUserQuestionItem } from "@/lib/session";

export type AskUserLabels = {
  title: string;
  submit: string;
  next: string;
  cancel: string;
  otherPlaceholder: string;
  freeTextHint: string;
  multiHint: string;
  close: string;
};

type Props = {
  payload: AskUserPayload | null;
  labels: AskUserLabels;
  onSubmit: (answers: Record<string, string>) => void | Promise<void>;
  onCancel: () => void | Promise<void>;
};

/** 按 ACP 问题标识生成提交答案。 */
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
    if (!optionIds.length) continue;
    answers[question.id] = optionIds
      .map((optionId) => {
        const option = question.options.find((item) => item.id === optionId);
        return option?.label || optionId;
      })
      .join(", ");
  }
  return answers;
}

export function AskUserModal({ payload, labels, onSubmit, onCancel }: Props) {
  const questions = payload?.questions ?? [];
  const open = Boolean(payload && questions.length > 0);

  // 按问题标识保存选中的选项标识，多选题允许多个值。
  const [selected, setSelected] = useState<Record<string, string[]>>({});
  // 按问题标识保存自由文本，自由文本优先于选项答案。
  const [freeText, setFreeText] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  // 收到新的提问请求时重置本地填写状态。
  useEffect(() => {
    if (!payload) {
      setSelected({});
      setFreeText({});
      setBusy(false);
      return;
    }
    setSelected({});
    setFreeText({});
    setBusy(false);
  }, [payload?.rpcId]);

  const canSubmit = useMemo(() => {
    if (!questions.length) return false;
    return questions.every((question) => {
      const text = (freeText[question.id] || "").trim();
      if (text) return true;
      const sel = selected[question.id] || [];
      return sel.length > 0;
    });
  }, [questions, selected, freeText]);

  /** 切换指定问题的选项状态。 */
  const toggleOption = (q: AskUserQuestionItem, optionId: string) => {
    const key = q.id;
    setSelected((prev) => {
      const cur = prev[key] || [];
      if (q.multiSelect) {
        const has = cur.includes(optionId);
        return {
          ...prev,
          [key]: has ? cur.filter((id) => id !== optionId) : [...cur, optionId],
        };
      }
      return { ...prev, [key]: [optionId] };
    });
    // 选择选项后清空该问题的自由文本。
    setFreeText((prev) => {
      if (!prev[key]) return prev;
      const next = { ...prev };
      delete next[key];
      return next;
    });
  };

  /** 提交当前答案。 */
  const submit = async (answers: Record<string, string>) => {
    if (busy) return;
    setBusy(true);
    try {
      await onSubmit(answers);
    } finally {
      setBusy(false);
    }
  };

  /** 取消当前提问。 */
  const cancel = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await onCancel();
    } finally {
      setBusy(false);
    }
  };

  // 单个单选题点击选项后立即提交。
  const quickPick =
    questions.length === 1 &&
    !questions[0]?.multiSelect &&
    (questions[0]?.options?.length ?? 0) > 0;

  return (
    <GlassModal
      open={open}
      onClose={() => void cancel()}
      title={labels.title}
      size="md"
      closeLabel={labels.close}
      closeOnOverlay={false}
      wrapBody
      footer={
        <>
          <Button
            type="button"
            className="btn btn--ghost"
            disabled={busy}
            onClick={() => void cancel()}
          >
            {labels.cancel}
          </Button>
          <Button
            type="button"
            className="btn btn--solid"
            disabled={busy || !canSubmit}
            onClick={() =>
              void submit(buildAskUserAnswers(questions, selected, freeText))
            }
          >
            {labels.submit}
          </Button>
        </>
      }
    >
      <div className="ask-user">
        {questions.map((q, qi) => {
          const key = q.id;
          const sel = selected[key] || [];
          const text = freeText[key] || "";
          return (
            <div
              key={q.id}
              className="ask-user__q"
              role="group"
              aria-labelledby={`ask-user-q-${qi}`}
            >
              <div className="ask-user__prompt" id={`ask-user-q-${qi}`}>
                {q.question}
              </div>
              {q.multiSelect ? (
                <div className="ask-user__hint" id={`ask-user-hint-${qi}`}>
                  {labels.multiHint}
                </div>
              ) : null}
              {q.options?.length ? (
                <div
                  className="ask-user__options"
                  role="group"
                  aria-labelledby={`ask-user-q-${qi}`}
                >
                  {q.options.map((opt) => {
                    const active = sel.includes(opt.id);
                    return (
                      <Button
                        key={opt.id}
                        type="button"
                        className={
                          "ask-user__opt" + (active ? " ask-user__opt--active" : "")
                        }
                        disabled={busy}
                        aria-pressed={active}
                        onClick={() => {
                          if (quickPick) {
                            void submit({ [q.id]: opt.label });
                            return;
                          }
                          toggleOption(q, opt.id);
                        }}
                      >
                        <span className="ask-user__opt-label">{opt.label}</span>
                        {opt.description ? (
                          <span className="ask-user__opt-desc">{opt.description}</span>
                        ) : null}
                      </Button>
                    );
                  })}
                </div>
              ) : null}
              <Label className="ask-user__free">
                <span className="ask-user__free-hint">
                  {q.options?.length ? labels.freeTextHint : labels.otherPlaceholder}
                </span>
                <Textarea
                  className="ask-user__textarea"
                  rows={2}
                  value={text}
                  disabled={busy}
                  placeholder={labels.otherPlaceholder}
                  aria-label={
                    q.options?.length
                      ? labels.freeTextHint
                      : labels.otherPlaceholder
                  }
                  onChange={(e) => {
                    const v = e.target.value;
                    setFreeText((prev) => ({ ...prev, [key]: v }));
                    if (v.trim() && !q.multiSelect) {
                      // 自由文本替换单选题的已选选项。
                      setSelected((prev) => ({ ...prev, [key]: [] }));
                    }
                  }}
                />
              </Label>
            </div>
          );
        })}
      </div>
    </GlassModal>
  );
}
