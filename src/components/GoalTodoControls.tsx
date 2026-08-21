import { useCallback, useEffect, useMemo, useState, type FormEvent } from "react";
import type { Locale } from "@/i18n";
import {
  goalUpsert,
  goalTransition,
} from "@/lib/acp/api";
import type { GoalRecordDto } from "@/lib/acp/events";
import type { AcpGoalProjection, AcpTodoProjection } from "@/lib/acp/store";
import {
  IconCheck,
  IconEdit,
  IconPlus,
  IconTarget,
} from "@/components/icons";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

/** Goal/Todo 控件使用的本地化文案。 */
interface GoalTodoLabels {
  goals: string;
  todos: string;
  newGoal: string;
  editGoal: string;
  goalTitle: string;
  goalDescription: string;
  save: string;
  cancel: string;
  status: string;
  applyStatus: string;
  activeGoal: string;
  successCriteria: string;
  nextSteps: string;
  blockers: string;
  emptyGoals: string;
  emptyTodos: string;
  revision: string;
  progress: string;
  tokensUsed: string;
}

function labelsForLocale(locale: Locale): GoalTodoLabels {
  if (locale === "zh") {
    return {
      goals: "目标",
      todos: "当前会话 Todo",
      newGoal: "新建目标",
      editGoal: "编辑目标",
      goalTitle: "目标标题",
      goalDescription: "目标描述",
      save: "保存",
      cancel: "取消",
      status: "状态",
      applyStatus: "应用状态",
      activeGoal: "当前目标",
      successCriteria: "成功标准",
      nextSteps: "下一步",
      blockers: "阻塞项",
      emptyGoals: "暂无目标",
      emptyTodos: "暂无 Todo",
      revision: "修订号",
      progress: "进度",
      tokensUsed: "已用 Token",
    };
  }
  return {
    goals: "Goals",
    todos: "Current todo",
    newGoal: "New goal",
    editGoal: "Edit goal",
    goalTitle: "Goal title",
    goalDescription: "Goal description",
    save: "Save",
    cancel: "Cancel",
    status: "Status",
    applyStatus: "Apply status",
    activeGoal: "Active goal",
    successCriteria: "Success criteria",
    nextSteps: "Next steps",
    blockers: "Blockers",
    emptyGoals: "No goals",
    emptyTodos: "No todos",
    revision: "Revision",
    progress: "Progress",
    tokensUsed: "Tokens used",
  };
}

/** Goal/Todo 类型化控件属性（ACP 契约）。 */
export interface GoalTodoControlsProps {
  /** 当前界面语言。 */
  locale: Locale;
  /** 所有 Goal 读写明确作用到的 Session。 */
  sessionId: string;
  /** 是否展示 Goal 分区。 */
  showGoals?: boolean;
  /** 当前权威 Goal 投影。 */
  goal: { revision: number; goal: GoalRecordDto | null };
  /** 是否展示 Todo 分区（只读 Plan）。 */
  showTodos?: boolean;
  /** 当前权威 Todo 投影（Plan 事件）。 */
  todos?: AcpTodoProjection;
  /** 把局部操作错误上报给父面板。 */
  onError?: (message: string) => void;
  /** Goal 保存或状态改变后把最新投影同步给工作台。 */
  onGoalChange?: (goal: AcpGoalProjection) => void;
}

const STATUS_OPTIONS = [
  { value: "active", label: "active" },
  { value: "completed", label: "completed" },
  { value: "blocked", label: "blocked" },
] as const;

export function isGoalStatus(value: string): value is GoalRecordDto["status"] {
  return STATUS_OPTIONS.some((option) => option.value === value);
}

/** Goal/Todo 控件（单例 Goal + 只读 Plan Todo）。 */
export function GoalTodoControls({
  locale,
  sessionId,
  showGoals = true,
  goal,
  showTodos = true,
  todos,
  onError,
  onGoalChange,
}: GoalTodoControlsProps) {
  const labels = useMemo(() => labelsForLocale(locale), [locale]);
  const [draftTitle, setDraftTitle] = useState("");
  const [draftDescription, setDraftDescription] = useState("");
  const [status, setStatus] = useState<GoalRecordDto["status"]>("active");
  const [busy, setBusy] = useState<string | null>(null);
  const [goalData, setGoalData] = useState(goal);

  // 跟随父面板传入的最新投影。
  useEffect(() => {
    setGoalData(goal);
  }, [goal]);

  const runWrite = useCallback(
    async (key: string, op: () => Promise<void>) => {
      setBusy(key);
      try {
        await op();
      } catch (cause) {
        onError?.(String(cause));
      } finally {
        setBusy(null);
      }
    },
    [onError],
  );

  const submitGoal = async (event: FormEvent) => {
    event.preventDefault();
    if (!draftTitle.trim()) return;
    await runWrite("goal:save", async () => {
      const result = await goalUpsert({
        sessionId,
        goal: { title: draftTitle.trim(), description: draftDescription.trim() },
        expectedRevision: goalData.goal ? goalData.revision : undefined,
        requestNonce: `keencode-${Date.now()}-${Math.random().toString(36).slice(2)}`,
      });
      setGoalData({ revision: result.revision, goal: result.goal });
      onGoalChange?.({ revision: result.revision, goal: result.goal });
      setDraftTitle("");
      setDraftDescription("");
    });
  };

  const applyStatus = async () => {
    const current = goalData.goal;
    if (!current) return;
    await runWrite("goal:status", async () => {
      const result = await goalTransition({
        sessionId,
        goalId: current.id,
        status,
        expectedRevision: goalData.revision,
      });
      setGoalData({ revision: result.revision, goal: result.goal });
      onGoalChange?.({ revision: result.revision, goal: result.goal });
    });
  };

  const startEdit = () => {
    const current = goalData.goal;
    setDraftTitle(current?.title ?? "");
    setDraftDescription(current?.description ?? "");
    setStatus(current?.status ?? "active");
  };

  const orderedTodos = useMemo(() => todos?.items ?? [], [todos]);

  return (
    <div className="goal-todo-controls">
      {showGoals && (
        <section className="goal-todo-controls__section">
          <h3 className="goal-todo-controls__heading">
            <IconTarget size={14} />
            {labels.goals}
            {goalData.goal && (
              <span className="goal-todo-controls__revision">
                {labels.revision} {goalData.revision}
              </span>
            )}
          </h3>

          {goalData.goal ? (
            <div className="goal-card">
              <div className="goal-card__head">
                <strong>{goalData.goal.title}</strong>
                <span className="goal-card__status goal-card__status--active">
                  {goalData.goal.status}
                </span>
                <button
                  type="button"
                  className="goal-card__edit"
                  title={labels.editGoal}
                  onClick={startEdit}
                >
                  <IconEdit size={13} />
                </button>
              </div>
              {goalData.goal.description ? (
                <p className="goal-card__description">
                  {goalData.goal.description}
                </p>
              ) : null}
              {typeof goalData.goal.progress_percent === "number" ? (
                <div className="goal-card__progress">
                  <span>
                    {labels.progress}: {Math.round(goalData.goal.progress_percent)}%
                  </span>
                  <div className="goal-card__progress-bar">
                    <div
                      className="goal-card__progress-fill"
                      style={{
                        width: `${Math.min(100, goalData.goal.progress_percent)}%`,
                      }}
                    />
                  </div>
                </div>
              ) : null}
              <div className="goal-card__meta">
                <span>
                  {labels.tokensUsed}: {goalData.goal.tokens_used}
                </span>
                {goalData.goal.blocked_reason ? (
                  <span className="goal-card__blockers">
                    {labels.blockers}: {goalData.goal.blocked_reason}
                  </span>
                ) : null}
              </div>

              <div className="goal-card__status-row">
                <Select
                  value={status}
                  onValueChange={(nextStatus) => {
                    if (isGoalStatus(nextStatus)) setStatus(nextStatus);
                  }}
                  disabled={busy !== null}
                >
                  <SelectTrigger
                    className="settings-input"
                    aria-label={labels.status}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      <SelectLabel>{labels.status}</SelectLabel>
                      {STATUS_OPTIONS.map((option) => (
                        <SelectItem key={option.value} value={option.value}>
                          {option.label}
                        </SelectItem>
                      ))}
                    </SelectGroup>
                  </SelectContent>
                </Select>
                <button
                  type="button"
                  className="btn btn--secondary"
                  disabled={busy !== null || status === goalData.goal.status}
                  onClick={() => void applyStatus()}
                >
                  {busy === "goal:status" ? "…" : labels.applyStatus}
                </button>
              </div>
            </div>
          ) : (
            <p className="goal-todo-controls__empty">{labels.emptyGoals}</p>
          )}

          <form className="goal-form" onSubmit={(e) => void submitGoal(e)}>
            <input
              className="settings-input"
              value={draftTitle}
              onChange={(event) => setDraftTitle(event.target.value)}
              placeholder={labels.goalTitle}
              disabled={busy !== null}
            />
            <input
              className="settings-input"
              value={draftDescription}
              onChange={(event) => setDraftDescription(event.target.value)}
              placeholder={labels.goalDescription}
              disabled={busy !== null}
            />
            <button
              type="submit"
              className="btn btn--primary"
              disabled={busy !== null || !draftTitle.trim()}
            >
              {busy === "goal:save" ? "…" : <IconPlus size={13} />}
              {goalData.goal ? labels.editGoal : labels.newGoal}
            </button>
          </form>
        </section>
      )}

      {showTodos && (
        <section className="goal-todo-controls__section">
          <h3 className="goal-todo-controls__heading">
            {labels.todos}
            {todos ? (
              <span className="goal-todo-controls__revision">
                {labels.revision} {todos.revision}
              </span>
            ) : null}
          </h3>
          {orderedTodos.length > 0 ? (
            <ul className="todo-list">
              {orderedTodos.map((todo, index) => (
                <li
                  key={`${todo.content}:${index}`}
                  className={`todo-item todo-item--${todo.status}`}
                >
                  {todo.status === "completed" ? (
                    <IconCheck size={13} />
                  ) : (
                    <span className="todo-item__dot" />
                  )}
                  <span>{todo.content}</span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="goal-todo-controls__empty">{labels.emptyTodos}</p>
          )}
        </section>
      )}
    </div>
  );
}
