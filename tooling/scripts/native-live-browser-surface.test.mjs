import assert from 'node:assert/strict';
import test from 'node:test';
import { cdpTargetIdentity, exactCdpTarget, sameCdpTarget, waitForStableCdpTarget } from './native-live-browser-surface.mjs';

const expected = 'http://127.0.0.1:8765/popup.html';
const target = (id, websocket = `ws://127.0.0.1/devtools/page/${id}`) => ({
  id, type: 'page', url: expected, webSocketDebuggerUrl: websocket,
});

test('同一 URL 的 CDP target 必须连续保持同一 id 与 endpoint', async () => {
  const oldTarget = target('old');
  const nextTarget = target('next');
  let snapshot = 0;
  let clock = 0;
  const stable = await waitForStableCdpTarget({
    expected,
    deadline: 10,
    now: () => clock,
    pause: async () => { clock += 1; },
    readTargets: async () => [snapshot++ === 0 ? oldTarget : nextTarget],
  });
  assert.equal(stable.id, 'next');
  assert.equal(cdpTargetIdentity(stable), 'next');
  assert.equal(exactCdpTarget([{ type: 'page', url: expected }, oldTarget], expected).id, 'old');
  assert.equal(sameCdpTarget(oldTarget, nextTarget), false);
  assert.equal(sameCdpTarget(nextTarget, { ...nextTarget, title: 'popup' }), true);
});

test('同 URL 的多个 target 不按列表顺序选择旧 child', () => {
  assert.equal(exactCdpTarget([target('old'), target('new')], expected), null);
});

test('稳定 target 的身份变化不会被 endpoint 缺失掩盖', () => {
  const left = target('same', '');
  const right = target('different', '');
  assert.equal(sameCdpTarget(left, right), false);
});

test('没有连续稳定 target 时在预算内失败', async () => {
  let clock = 0;
  await assert.rejects(
    waitForStableCdpTarget({
      expected,
      deadline: 2,
      now: () => clock,
      pause: async () => { clock += 1; },
      readTargets: async () => [target(`rotating-${clock}`)],
    }),
    /未加载稳定/,
  );
});
