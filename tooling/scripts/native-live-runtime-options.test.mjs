import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import {
  applyRuntimeArtifactLimit,
  MAX_RUNTIME_ARTIFACT_LIMIT,
  parseRuntimeArtifactLimit,
  RUNTIME_ARTIFACT_LIMIT_ENV,
} from './native-live-runtime-options.mjs';

test('runtimeArtifactLimit 只接受 1..8192 的 JSON 整数，省略值不设覆盖', () => {
  assert.equal(parseRuntimeArtifactLimit(undefined), undefined);
  assert.equal(parseRuntimeArtifactLimit(2048), 2048);
  for (const value of ['2048', 2048.5, 0, MAX_RUNTIME_ARTIFACT_LIMIT + 1, null, NaN, Infinity]) {
    assert.throws(() => parseRuntimeArtifactLimit(value), /1\.\.8192/);
  }
});

test('省略计划值会清除继承覆盖，benchmark 隔离进程只注入实际数值', () => {
  const inherited = {
    KEENCODE_BENCHMARK: '1',
    [RUNTIME_ARTIFACT_LIMIT_ENV]: '8193',
  };
  assert.equal(applyRuntimeArtifactLimit(inherited, undefined)[RUNTIME_ARTIFACT_LIMIT_ENV], undefined);
  assert.equal(applyRuntimeArtifactLimit(inherited, 2048)[RUNTIME_ARTIFACT_LIMIT_ENV], '2048');
  assert.equal(
    applyRuntimeArtifactLimit({ KEENCODE_BENCHMARK: '0', [RUNTIME_ARTIFACT_LIMIT_ENV]: '17' }, 2048)[RUNTIME_ARTIFACT_LIMIT_ENV],
    undefined,
  );
});

test('workflow controls 使用 2048 容量且保留 1024 次只读 Read', () => {
  const plan = JSON.parse(readFileSync(new URL('../native-live/workflow-controls-plan.json', import.meta.url), 'utf8'));
  const workflow = JSON.parse(readFileSync(new URL('../native-live/recoverable-read.workflow.json', import.meta.url), 'utf8'));
  const repeat = workflow.body.find((node) => node.type === 'repeat' && node.node_id === 'queued_reads');
  assert.equal(plan.runtimeArtifactLimit, 2048);
  assert.equal(repeat?.max_iterations, 1024);
});
