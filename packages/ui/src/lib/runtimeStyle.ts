/**
 * 为运行时创建的 style 复用宿主注入到 bundled inline 资源上的 nonce。
 * Tauri 的 CSP 会拒绝没有 nonce 的动态 style；开发服务器没有 nonce 时保留原有行为。
 */
export function applyBundledStyleNonce(document: Document, style: HTMLStyleElement): void {
  const nonce = Array.from(
    document.head.querySelectorAll<HTMLStyleElement>("style"),
  )
    .filter((element) => element !== style)
    .map((element) => element.nonce)
    .find((value) => value.length > 0) ?? style.nonce;
  if (nonce && style.nonce !== nonce) {
    style.nonce = nonce;
  }
}

export function createRuntimeStyleElement(document: Document): HTMLStyleElement {
  const style = document.createElement("style");
  applyBundledStyleNonce(document, style);
  return style;
}
