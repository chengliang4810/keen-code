export const MAX_PAGE_PROBE_BYTES = 128 * 1024;

/**
 * 页面 probe 只允许写有限的 JSON 诊断快照；文件名经过单向清理，避免
 * 验收计划把输出路径带出本次隔离目录。脱敏回调由 runner 提供。
 */
export function serializePageProbe(name, value, redact = (text) => text) {
  if (typeof name !== 'string' || name.trim().length === 0) {
    throw new Error('页面 probe 缺少名称');
  }
  if (typeof redact !== 'function') throw new Error('页面 probe 脱敏器无效');
  const serialized = JSON.stringify(value, null, 2);
  if (serialized === undefined) throw new Error('页面 probe 结果不可序列化');
  const bytes = Buffer.from(redact(serialized), 'utf8');
  if (bytes.byteLength > MAX_PAGE_PROBE_BYTES) {
    throw new Error('页面 probe 结果无效或超出大小限制');
  }
  const filename = name.replace(/[^a-zA-Z0-9_-]/g, '_');
  return { filename, bytes };
}
