/** 只识别协议与投影故障；真实模型失败和普通业务拒绝由各验收步骤独立判断。 */
export function nativeProtocolFaults(diagnostics) {
  const patterns = [
    /frontend RPC request failed.*code="(?:fault\.subscription\.notOwned|rpc\.unsupportedService|fault\.(?:snapshot|frame|subscription)\.[^"]+)"/,
    /"event":"v4\.conversation\.(?:unsubscribe\.failed|store\.close\.unsubscribe_failed|frame\.(?:decode|apply)\.failed)"/,
  ];
  return String(diagnostics).split(/\r?\n/).filter((line) => patterns.some((pattern) => pattern.test(line))).slice(-100);
}

/** 未处理的渲染异常必须使验收失败；重载期间 Tauri 的迟到回调提示另存诊断。 */
export function nativeFrontendFaults(errors) {
  return errors.filter((error) => /^(?:Uncaught\s+)?(?:Error|TypeError|ReferenceError|RangeError|SyntaxError|EvalError|URIError):/.test(String(error)));
}
