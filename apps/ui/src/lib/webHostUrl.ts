/** Web Host 对外地址的纯规则；配对 URL 一律由这里构造，不散落手写格式。 */

const LOOPBACK_HOSTS = new Set(["127.0.0.1", "::1", "localhost", "[::1]"]);

/** IPv6 字面量放进 URL host 时需要方括号。 */
export function displayWebHostHost(bind: string): string {
  const value = bind.trim();
  return value.includes(":") && !value.startsWith("[") ? `[${value}]` : value;
}

/** 回环监听地址只有本机能访问，手机无法直连。 */
export function isLoopbackBind(bind: string): boolean {
  return LOOPBACK_HOSTS.has(bind.trim().toLowerCase());
}

/**
 * 构造手机扫码/打开链接使用的配对 URL；token 只经这条 URL 一次性送达，
 * 由 HostStartupShell 消费后立即从地址栏清除。
 */
export function buildMobileRemoteUrl(
  bind: string,
  port: number,
  token: string,
): string | null {
  const host = displayWebHostHost(bind);
  const trimmedToken = token.trim();
  if (!host || !trimmedToken) return null;
  if (!Number.isInteger(port) || port <= 0 || port > 65_535) return null;
  const params = new URLSearchParams({ host: "mobile-remote", token: trimmedToken });
  return `http://${host}:${port}/?${params.toString()}`;
}
