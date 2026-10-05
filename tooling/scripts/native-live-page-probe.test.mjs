import test from 'node:test';
import assert from 'node:assert/strict';
import {
  MAX_PAGE_PROBE_BYTES,
  serializePageProbe,
} from './native-live-page-probe.mjs';

test('页面 probe 序列化清理文件名并执行最终脱敏', () => {
  const result = serializePageProbe('../probe value', { token: 'private-value' }, (text) =>
    text.replaceAll('private-value', '[redacted]'));
  assert.equal(result.filename, '___probe_value');
  assert.ok(!result.bytes.toString('utf8').includes('private-value'));
  assert.match(result.bytes.toString('utf8'), /\[redacted\]/u);
});

test('页面 probe 拒绝不可序列化值并按 UTF-8 字节数执行上限', () => {
  assert.throws(() => serializePageProbe('undefined', undefined), /不可序列化/u);
  const cyclic = {};
  cyclic.self = cyclic;
  assert.throws(() => serializePageProbe('cyclic', cyclic), TypeError);
  const multibyte = '界'.repeat(MAX_PAGE_PROBE_BYTES);
  assert.throws(() => serializePageProbe('large', multibyte), /大小限制/u);
  const valid = serializePageProbe('limit', 'x'.repeat(MAX_PAGE_PROBE_BYTES - 4));
  assert.ok(valid.bytes.byteLength <= MAX_PAGE_PROBE_BYTES);
});
