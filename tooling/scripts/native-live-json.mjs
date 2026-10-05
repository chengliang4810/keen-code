import { createHash } from 'node:crypto';

function schemaTypeMatches(value, type) {
  switch (type) {
    case 'object': return value !== null && typeof value === 'object' && !Array.isArray(value);
    case 'array': return Array.isArray(value);
    case 'string': return typeof value === 'string';
    case 'integer': return Number.isSafeInteger(value);
    case 'number': return typeof value === 'number' && Number.isFinite(value);
    case 'boolean': return typeof value === 'boolean';
    case 'null': return value === null;
    default: throw new Error('JSON schema type 不受支持');
  }
}

/** 只实现验收计划实际需要的 JSON Schema 子集，失败只返回脱敏字段位置。 */
export function validateJsonSchema(value, schema, path = '$') {
  if (!schema || typeof schema !== 'object' || Array.isArray(schema)) {
    throw new Error('JSON schema 必须为对象');
  }
  if (schema.type !== undefined) {
    const types = Array.isArray(schema.type) ? schema.type : [schema.type];
    if (!types.some((type) => schemaTypeMatches(value, type))) throw new Error(`JSON schema 类型失败: ${path}`);
  }
  if (schema.enum !== undefined && (!Array.isArray(schema.enum)
      || !schema.enum.some((candidate) => Object.is(candidate, value)))) {
    throw new Error(`JSON schema 枚举失败: ${path}`);
  }
  if (schema.required !== undefined) {
    if (!Array.isArray(schema.required) || !schema.required.every((key) => typeof key === 'string')) {
      throw new Error(`JSON schema required 无效: ${path}`);
    }
    for (const key of schema.required) {
      if (!Object.prototype.hasOwnProperty.call(value ?? {}, key)) throw new Error(`JSON schema 缺少字段: ${path}.${key}`);
    }
  }
  if (schema.properties !== undefined) {
    if (!schemaTypeMatches(value, 'object') || typeof schema.properties !== 'object' || schema.properties === null) {
      throw new Error(`JSON schema properties 无效: ${path}`);
    }
    for (const [key, childSchema] of Object.entries(schema.properties)) {
      if (Object.prototype.hasOwnProperty.call(value, key)) validateJsonSchema(value[key], childSchema, `${path}.${key}`);
    }
    if (schema.additionalProperties === false) {
      const allowed = new Set(Object.keys(schema.properties));
      for (const key of Object.keys(value)) if (!allowed.has(key)) throw new Error(`JSON schema 未知字段: ${path}.${key}`);
    }
  }
  if (schema.minProperties !== undefined && (!schemaTypeMatches(value, 'object')
      || Object.keys(value).length < schema.minProperties)) throw new Error(`JSON schema 对象为空: ${path}`);
  if (schema.minItems !== undefined && (!Array.isArray(value) || value.length < schema.minItems)) {
    throw new Error(`JSON schema 数组为空: ${path}`);
  }
  if (schema.minLength !== undefined && (typeof value !== 'string' || value.length < schema.minLength)) {
    throw new Error(`JSON schema 字符串为空: ${path}`);
  }
  return true;
}

function valueAtPath(value, path) {
  const parts = String(path).replace(/^\$\.?/u, '').split('.').filter(Boolean);
  return parts.reduce((current, part) => current?.[part], value);
}

function hasForbiddenKey(value, forbiddenNames) {
  if (Array.isArray(value)) return value.some((item) => hasForbiddenKey(item, forbiddenNames));
  if (!value || typeof value !== 'object') return false;
  for (const [key, child] of Object.entries(value)) {
    if (forbiddenNames.has(key.toLowerCase())) return true;
    if (hasForbiddenKey(child, forbiddenNames)) return true;
  }
  return false;
}

function compileForbiddenPattern(source) {
  if (typeof source !== 'string' || !source) throw new Error('JSON 敏感值模式计划无效');
  let pattern = source;
  let flags = 'u';
  // 计划来源使用 PCRE 风格的行内大小写标记；JS RegExp 不接受该语法，
  // 这里只允许出现在开头并转换为 JS 的 i flag，避免放宽任意表达式。
  if (pattern.startsWith('(?i)')) {
    pattern = pattern.slice(4);
    flags += 'i';
  }
  try {
    return new RegExp(pattern, flags);
  } catch {
    throw new Error('JSON 敏感值模式计划无效');
  }
}

/** 校验隔离导出的 JSON；只返回摘要，不把 JSON 正文写入报告。 */
export function validateJsonEvidence(bytes, {
  schema,
  expectedSchema,
  requiredTopLevelKeys = [],
  requiredNonEmpty = [],
  secrets = [],
  forbiddenKeyNames = [],
  forbiddenValuePatterns = [],
  minBytes = 1,
  maxBytes = 256 * 1024,
} = {}) {
  if (!(bytes instanceof Uint8Array) || bytes.byteLength < minBytes || bytes.byteLength > maxBytes) {
    throw new Error('JSON 导出字节大小不符合计划');
  }
  const text = Buffer.from(bytes).toString('utf8');
  if (!text.trim()) throw new Error('JSON 导出为空');
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Error('JSON 导出格式无效');
  }
  if (schema) validateJsonSchema(value, schema);
  if (expectedSchema !== undefined
      && (!Number.isSafeInteger(expectedSchema) || value?.schema !== expectedSchema)) {
    throw new Error('JSON 导出 schema 版本不匹配');
  }
  if (!Array.isArray(requiredTopLevelKeys)
      || requiredTopLevelKeys.some((key) => typeof key !== 'string' || !key.trim())) {
    throw new Error('JSON 顶层字段计划无效');
  }
  for (const key of requiredTopLevelKeys) {
    if (!value || typeof value !== 'object' || Array.isArray(value)
        || !Object.prototype.hasOwnProperty.call(value, key)) {
      throw new Error('JSON 导出缺少必需字段');
    }
  }
  if (!Array.isArray(requiredNonEmpty) || requiredNonEmpty.some((path) => typeof path !== 'string' || !path.trim())) {
    throw new Error('JSON 非空字段计划无效');
  }
  for (const path of requiredNonEmpty) {
    const field = valueAtPath(value, path);
    if (field === undefined || field === null || field === ''
        || (Array.isArray(field) && field.length === 0)
        || (typeof field === 'object' && !Array.isArray(field) && Object.keys(field).length === 0)) {
      throw new Error(`JSON 非空字段失败: ${path}`);
    }
  }
  if (!Array.isArray(forbiddenKeyNames)
      || forbiddenKeyNames.some((key) => typeof key !== 'string' || !key.trim())) {
    throw new Error('JSON 禁止字段计划无效');
  }
  const forbiddenNames = new Set(forbiddenKeyNames.map((key) => key.toLowerCase()));
  if (hasForbiddenKey(value, forbiddenNames)) throw new Error('JSON 导出包含禁止字段');
  const serialized = text;
  if (secrets.some((secret) => typeof secret === 'string' && secret.length > 0 && serialized.includes(secret))) {
    throw new Error('JSON 导出包含已配置敏感值');
  }
  if (!Array.isArray(forbiddenValuePatterns)) throw new Error('JSON 敏感值模式计划无效');
  const forbiddenPatterns = forbiddenValuePatterns.map(compileForbiddenPattern);
  if (forbiddenPatterns.some((pattern) => pattern.test(serialized))) {
    throw new Error('JSON 导出包含禁止敏感值');
  }
  return {
    sizeBytes: bytes.byteLength,
    sha256: createHash('sha256').update(bytes).digest('hex'),
    jsonValid: true,
    schemaValid: true,
    requiredNonEmpty: true,
    configuredSecretsAbsent: true,
    forbiddenKeysAbsent: true,
    forbiddenValuePatternsAbsent: true,
  };
}
