import type { AppSettings, AppSettingsPatch } from "./api";

/** AppSettings 当前允许局部持久化的字段。 */
export type AppSettingKey = keyof AppSettingsPatch;

/** 单个设置字段的顺序持久化状态。 */
export interface AppSettingPersistenceEntry {
  /** 当前字段最近一次乐观更新的单调修订号。 */
  revision: number;
  /** 后端最后一次确认成功的界面状态。 */
  confirmed: unknown;
  /** 当前字段的顺序写入尾任务，防止旧请求晚落盘覆盖新值。 */
  tail: Promise<void>;
}

/** 所有设置字段各自隔离的顺序持久化状态。 */
export type AppSettingPersistenceMap = Map<
  AppSettingKey,
  AppSettingPersistenceEntry
>;

/** 单次类型安全的设置字段乐观更新。 */
export interface LatestAppSettingUpdate<
  Key extends AppSettingKey,
  State,
> {
  /** 需要持久化的 AppSettings 字段。 */
  key: Key;
  /** 发送给后端的当前字段值。 */
  value: AppSettings[Key];
  /** 请求发出前立即展示的界面状态。 */
  optimistic: State;
  /** 最新请求失败时恢复的界面状态。 */
  previous: State;
  /** 把界面状态提交给 React。 */
  apply: (value: State) => void;
  /** 把后端规范化后的完整设置映射回当前字段状态。 */
  normalizeSaved?: (settings: AppSettings) => State;
}

/** 设置持久化所需的可替换依赖，供 Hook 与测试共享。 */
export interface AppSettingPersistenceDependencies {
  /** 按字段保存修订号、已确认值和顺序写入尾任务。 */
  states: AppSettingPersistenceMap;
  /** 将单字段补丁写入后端。 */
  persist: (patch: AppSettingsPatch) => Promise<AppSettings>;
  /** 最新请求保存失败时报告一次界面错误。 */
  onError: () => void;
}

/**
 * 乐观保存一个设置字段；过期请求不得回滚新状态或应用旧规范化结果。
 */
export async function persistLatestAppSetting<
  Key extends AppSettingKey,
  State,
>(
  update: LatestAppSettingUpdate<Key, State>,
  dependencies: AppSettingPersistenceDependencies,
): Promise<void> {
  let state = dependencies.states.get(update.key);
  if (!state) {
    state = {
      revision: 0,
      confirmed: update.previous,
      tail: Promise.resolve(),
    };
    dependencies.states.set(update.key, state);
  }
  const revision = state.revision + 1;
  state.revision = revision;
  update.apply(update.optimistic);

  const operation = state.tail.then(async () => {
    try {
      const saved = await dependencies.persist({
        [update.key]: update.value,
      } as AppSettingsPatch);
      const confirmed = update.normalizeSaved
        ? update.normalizeSaved(saved)
        : update.optimistic;
      state.confirmed = confirmed;
      if (state.revision === revision && update.normalizeSaved) {
        update.apply(confirmed);
      }
    } catch {
      if (state.revision !== revision) return;
      update.apply(state.confirmed as State);
      dependencies.onError();
    }
  });
  state.tail = operation;
  await operation;
}
