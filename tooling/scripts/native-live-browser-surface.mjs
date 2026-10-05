/**
 * 原生子 WebView 的 CDP target 选择规则。
 * WebView2 在导航/重建期间可能短暂同时返回同一 URL 的旧、新 target；验收必须
 * 看到同一身份连续出现两次后才连接，避免把旧 websocket 当成当前原生表面。
 */
export function cdpTargetIdentity(target) {
  const id = target?.targetId ?? target?.id;
  if (typeof id === 'string' && id.length > 0) return id;
  const websocket = target?.webSocketDebuggerUrl;
  return typeof websocket === 'string' && websocket.length > 0 ? websocket : null;
}

export function exactCdpTarget(targets, expected) {
  const matches = (Array.isArray(targets) ? targets : []).filter((target) =>
    target?.type === 'page' && target.url === expected && cdpTargetIdentity(target));
  // 两个同 URL target 同时存在时不能凭数组顺序选择旧 child。
  return matches.length === 1 ? matches[0] : null;
}

/** 同一 CDP target 必须保持 target id、URL 和 websocket endpoint 一致。 */
export function sameCdpTarget(left, right) {
  if (!left || !right || left.type !== 'page' || right.type !== 'page'
      || left.url !== right.url) return false;
  const leftIdentity = cdpTargetIdentity(left);
  const rightIdentity = cdpTargetIdentity(right);
  if (!leftIdentity || leftIdentity !== rightIdentity) return false;
  const leftWebsocket = left.webSocketDebuggerUrl;
  const rightWebsocket = right.webSocketDebuggerUrl;
  return !leftWebsocket || !rightWebsocket || leftWebsocket === rightWebsocket;
}

export async function waitForStableCdpTarget({ readTargets, expected, deadline,
  pause, now = Date.now }) {
  let previous = null;
  while (now() < deadline) {
    const current = exactCdpTarget(await readTargets(), expected);
    if (current && previous && sameCdpTarget(previous, current)) return current;
    previous = current ?? null;
    const remaining = deadline - now();
    if (remaining <= 0) break;
    await pause(Math.min(150, remaining));
  }
  throw new Error('原生子 WebView 未加载稳定的指定夹具页面');
}
