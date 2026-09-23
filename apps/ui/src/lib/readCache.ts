/**
 * 设置面板等「挂载即读取」场景的短时缓存。
 *
 * 背景：设置页各分区面板在挂载时都会读取一次后端数据。切换分区会卸载并
 * 重建面板，于是每次都重新请求，用户感知为「每次打开都要重新加载」。
 *
 * 这里提供与 `gitStatus` 相同的 stale-while-revalidate 语义：
 * - 缓存未过期时直接返回上次结果，界面立即可用（无空白闪烁）；
 * - 并发调用复用同一 Promise，避免重复请求；
 * - 显式失效用于写操作后刷新。
 *
 * 缓存只存在于渲染进程内存中，不改变后端权威状态。
 */

/** 一个缓存条目：上次成功结果与过期时间。 */
interface CacheEntry<T> {
  value: T;
  expiresAt: number;
}

/** 按 key 保存的读取缓存。 */
const entries = new Map<string, CacheEntry<unknown>>();
/** 按 key 保存的进行中请求，用于并发合并。 */
const inFlight = new Map<string, Promise<unknown>>();

/** 默认缓存有效期：覆盖分区切换的常见间隔，又不会让数据明显过期。 */
export const DEFAULT_READ_CACHE_TTL_MS = 30_000;

/** 读取选项。 */
export interface CachedReadOptions {
  /** 跳过缓存并强制请求；进行中的请求会被复用。 */
  force?: boolean;
  /** 自定义有效期；省略时使用 `DEFAULT_READ_CACHE_TTL_MS`。 */
  ttlMs?: number;
}

/**
 * 读取一个可缓存结果：命中未过期缓存直接返回，否则发起请求并写入缓存。
 *
 * `key` 必须唯一标识输入；不同输入（如项目路径、查询条件）应使用不同 key。
 */
export function cachedRead<T>(
  key: string,
  load: () => Promise<T>,
  options: CachedReadOptions = {},
): Promise<T> {
  const existing = inFlight.get(key);
  if (existing) {
    return existing as Promise<T>;
  }
  if (!options.force) {
    const cached = entries.get(key) as CacheEntry<T> | undefined;
    if (cached && cached.expiresAt > Date.now()) {
      return Promise.resolve(cached.value);
    }
  }
  const request = load()
    .then((value) => {
      entries.set(key, {
        value,
        expiresAt: Date.now() + (options.ttlMs ?? DEFAULT_READ_CACHE_TTL_MS),
      });
      return value;
    })
    .finally(() => {
      // 仅清理本次请求，避免清掉后来者写入的条目。
      if (inFlight.get(key) === request) {
        inFlight.delete(key);
      }
    });
  inFlight.set(key, request);
  return request;
}

/** 使一个 key 的缓存失效（写操作后调用）。 */
export function invalidateReadCache(key: string): void {
  entries.delete(key);
  inFlight.delete(key);
}

/** 使全部缓存失效；用于跨面板的批量变更（如导入配置）。 */
export function invalidateAllReadCaches(): void {
  entries.clear();
  inFlight.clear();
}
