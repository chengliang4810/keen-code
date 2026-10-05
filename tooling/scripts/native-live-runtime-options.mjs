export const RUNTIME_ARTIFACT_LIMIT_ENV = 'KEENCODE_NATIVE_TEST_MAX_ARTIFACTS_PER_SESSION';
export const MAX_RUNTIME_ARTIFACT_LIMIT = 8192;

/**
 * 计划省略容量时返回 undefined；显式值必须是 JSON number 的安全整数，
 * 避免把字符串、分数或越界值悄悄传给 native 测试进程。
 */
export function parseRuntimeArtifactLimit(value) {
  if (value === undefined) return undefined;
  if (!Number.isSafeInteger(value) || value < 1 || value > MAX_RUNTIME_ARTIFACT_LIMIT) {
    throw new Error(`runtimeArtifactLimit 必须是 1..${MAX_RUNTIME_ARTIFACT_LIMIT} 的整数`);
  }
  return value;
}

/**
 * 每次启动先清除父进程遗留覆盖；只有隔离 benchmark 进程才接收计划值。
 * 这样省略字段的计划不会继承上一组验收的 artifact 容量。
 */
export function applyRuntimeArtifactLimit(environment, limit) {
  const next = { ...environment };
  delete next[RUNTIME_ARTIFACT_LIMIT_ENV];
  if (limit !== undefined && next.KEENCODE_BENCHMARK === '1') {
    next[RUNTIME_ARTIFACT_LIMIT_ENV] = String(limit);
  }
  return next;
}
