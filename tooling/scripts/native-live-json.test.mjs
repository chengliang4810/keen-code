import test from 'node:test';
import assert from 'node:assert/strict';
import { validateJsonEvidence } from './native-live-json.mjs';

const schema = {
  type: 'object',
  required: ['schema', 'capturedAtMs', 'counters'],
  properties: {
    schema: { type: 'integer', enum: [1] },
    capturedAtMs: { type: 'integer' },
    counters: { type: 'object' },
  },
  additionalProperties: false,
};

test('JSON evidence 通过 schema、非空、大小和敏感值负向校验', () => {
  const bytes = Buffer.from(JSON.stringify({ schema: 1, capturedAtMs: 42, counters: { startup: 1 } }));
  const result = validateJsonEvidence(bytes, {
    schema,
    expectedSchema: 1,
    requiredTopLevelKeys: ['schema', 'capturedAtMs', 'counters'],
    requiredNonEmpty: ['counters'],
    secrets: ['https://example.invalid', 'secret-key'],
    forbiddenKeyNames: ['apiKey', 'authorization'],
    forbiddenValuePatterns: ['(?i)bearer\\s+[A-Za-z0-9._~+/=-]+'],
  });
  assert.equal(result.jsonValid, true);
  assert.equal(result.schemaValid, true);
  assert.equal(result.configuredSecretsAbsent, true);
  assert.equal(result.forbiddenKeysAbsent, true);
  assert.equal(result.forbiddenValuePatternsAbsent, true);
  assert.equal(result.sha256.length, 64);
});

test('JSON evidence 拒绝未知字段、空导出和配置敏感值', () => {
  assert.throws(() => validateJsonEvidence(Buffer.from(JSON.stringify({
    schema: 1, capturedAtMs: 42, counters: { startup: 1 }, extra: true,
  })), { schema }), /未知字段/);
  assert.throws(() => validateJsonEvidence(Buffer.from('   '), { schema }), /为空/);
  assert.throws(() => validateJsonEvidence(Buffer.from(JSON.stringify({
    schema: 1, capturedAtMs: 42, counters: { startup: 'secret-key' },
  })), { schema, secrets: ['secret-key'] }), /敏感值/);
  assert.throws(() => validateJsonEvidence(Buffer.from(JSON.stringify({
    schema: 1, capturedAtMs: 42, counters: { startup: 1 }, apiKey: 'redacted',
  })), {
    schema: { ...schema, additionalProperties: true },
    forbiddenKeyNames: ['apiKey'],
  }), /禁止字段/);
  assert.throws(() => validateJsonEvidence(Buffer.from(JSON.stringify({
    schema: 1, capturedAtMs: 42, counters: { startup: 'Bearer abc123' },
  })), {
    schema,
    forbiddenValuePatterns: ['(?i)bearer\\s+[A-Za-z0-9._~+/=-]+'],
  }), /禁止敏感值/);
});
