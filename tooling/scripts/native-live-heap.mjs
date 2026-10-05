/** 原生验收堆快照只保存已脱敏的字符串表，不把 provider 凭据写入证据。 */
export function redactHeapSnapshot(snapshot, secrets) {
  if (!Array.isArray(snapshot?.strings)) throw new Error('堆快照缺少字符串表');
  return {
    ...snapshot,
    strings: snapshot.strings.map((value) => secrets.filter(Boolean).reduce(
      (text, secret) => text.split(secret).join('[redacted]'), String(value),
    )),
  };
}

/** 按 V8 自带字段表解释节点，统计的是 V8 堆，不能当作整个 WebView2 的 RSS。 */
export function summarizeHeapSnapshot(snapshot) {
  const fields = snapshot?.snapshot?.meta?.node_fields;
  const types = snapshot?.snapshot?.meta?.node_types?.[0];
  const nodes = snapshot?.nodes;
  if (!Array.isArray(fields) || !Array.isArray(types) || !Array.isArray(nodes)
      || !['type', 'name', 'self_size'].every((field) => fields.includes(field))
      || nodes.length % fields.length !== 0) throw new Error('堆快照节点格式无效');
  const typeIndex = fields.indexOf('type');
  const nameIndex = fields.indexOf('name');
  const sizeIndex = fields.indexOf('self_size');
  const byType = new Map();
  const byObject = new Map();
  let totalSelfBytes = 0;
  const add = (map, name, size) => {
    const value = map.get(name) ?? { name, count: 0, selfBytes: 0 };
    value.count += 1; value.selfBytes += size; map.set(name, value);
  };
  for (let offset = 0; offset < nodes.length; offset += fields.length) {
    const type = types[nodes[offset + typeIndex]];
    const size = nodes[offset + sizeIndex];
    if (typeof type !== 'string' || !Number.isFinite(size) || size < 0) throw new Error('堆节点指标无效');
    totalSelfBytes += size;
    add(byType, type, size);
    if (type === 'object' || type === 'native') {
      add(byObject, snapshot.strings[nodes[offset + nameIndex]] ?? '(unknown)', size);
    }
  }
  const sort = (map) => [...map.values()].sort((left, right) => right.selfBytes - left.selfBytes);
  return { nodeCount: nodes.length / fields.length, totalSelfBytes, byType: sort(byType), largestObjects: sort(byObject).slice(0, 25) };
}
