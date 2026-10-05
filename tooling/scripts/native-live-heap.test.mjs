import assert from 'node:assert/strict';
import test from 'node:test';
import { redactHeapSnapshot, summarizeHeapSnapshot } from './native-live-heap.mjs';

const fixture = () => ({ snapshot: { meta: { node_fields: ['type', 'name', 'self_size'], node_types: [['object', 'string']] } }, nodes: [0, 0, 20, 1, 1, 10], strings: ['Object', 'a secret\"key b'] });

test('堆证据脱敏处理解码后的凭据，不修改输入快照', () => {
  const raw = fixture();
  const clean = redactHeapSnapshot(raw, ['secret"key']);
  assert.equal(clean.strings[1], 'a [redacted] b');
  assert.equal(raw.strings[1], 'a secret"key b');
  assert.ok(!JSON.stringify(clean).includes('secret'));
});

test('堆分析区分字符串与对象，不把字符串内容当作构造器名称输出', () => {
  const summary = summarizeHeapSnapshot(fixture());
  assert.equal(summary.nodeCount, 2);
  assert.equal(summary.totalSelfBytes, 30);
  assert.equal(summary.largestObjects.length, 1);
  assert.equal(summary.largestObjects[0].name, 'Object');
  assert.ok(!JSON.stringify(summary).includes('secret'));
  assert.throws(() => summarizeHeapSnapshot({ nodes: [] }), /格式无效/);
});
