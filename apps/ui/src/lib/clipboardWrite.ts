/**
 * 在用户手势的同步路径内把异步取得的文本写入系统剪贴板。
 *
 * WebKit 只放行手势未中断时的剪贴板写入：先 `await` 一次后端往返再调用
 * `writeText`，手势已经过期，写入会以 NotAllowedError 失败。这里把待取文本
 * 的 Promise 交给 ClipboardItem，让 `write()` 仍在点击回调的同步路径内发起。
 */

/**
 * 复制 `read()` 返回的文本。
 *
 * `read` 在调用时同步执行；调用方必须在点击回调中直接调用本函数，不得先
 * `await` 其他异步操作，否则手势过期后 WebKit 会拒绝写入。
 */
export async function copyTextInGesture(
  read: () => Promise<string>,
): Promise<void> {
  const text = read();
  const blob = text.then((value) => new Blob([value], { type: "text/plain" }));
  // write() 可能在数据就绪前失败，避免派生 Promise 留下未处理的拒绝。
  void blob.catch(() => {});

  try {
    await navigator.clipboard.write([new ClipboardItem({ "text/plain": blob })]);
  } catch (writeError) {
    // 文本读取失败时优先报告读取原因，避免被剪贴板错误掩盖。
    const readError = await text.then(
      () => null,
      (error: unknown) => error,
    );
    throw readError ?? writeError;
  }
}
