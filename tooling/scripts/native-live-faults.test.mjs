import assert from 'node:assert/strict';
import test from 'node:test';
import { nativeFrontendFaults, nativeProtocolFaults } from './native-live-faults.mjs';

test('原生报告不能把被 renderer 捕获的订阅归属故障算作通过', () => {
  const log = 'level=warn component=backend message=frontend RPC request failed channel="zcode-agent" method="unsubscribeConversationV4" code="fault.subscription.notOwned"\n'
    + 'level=error component=renderer.warn message=warn: v4 conversation unsubscription failed {"event":"v4.conversation.unsubscribe.failed"}';
  assert.equal(nativeProtocolFaults(log).length, 2);
});

test('真实模型失败、权限拒绝与成功释放不是协议故障', () => {
  const log = 'level=error component=backend ModelFailure: provider request timed out\n'
    + 'frontend RPC request failed code="fault.permission.denied"\n'
    + '{"event":"v4.conversation.unsubscribe.completed"}';
  assert.deepEqual(nativeProtocolFaults(log), []);
});

test('真实浏览器渲染循环和监听器异常不能由完成步骤掩盖', () => {
  const errors = ['Error: Minified React error #185; maximum update depth exceeded',
    "TypeError: Cannot read properties of undefined (reading 'handlerId')"];
  assert.deepEqual(nativeFrontendFaults(errors), errors);
});

test('重载的迟到回调提示和带业务前缀的请求失败独立记录', () => {
  assert.deepEqual(nativeFrontendFaults([
    "[TAURI] Couldn't find callback id 1. This might happen when the app is reloaded",
    '[Provider] request timed out',
  ]), []);
});
