/** 对已经脱敏的原生 CPU profile 汇总采样权重，区分等待与实际 JS/GC 占用。 */
import { readFile, writeFile } from 'node:fs/promises';
const file = process.argv[2];
if (!file) throw new Error('需要已保存的 .cpuprofile 路径');
const profile = JSON.parse(await readFile(file, 'utf8'));
const nodes = new Map(profile.nodes.map((node) => [node.id, node]));
const times = new Map();
for (let i = 0; i < (profile.samples ?? []).length; i++) {
  const id = profile.samples[i];
  times.set(id, (times.get(id) ?? 0) + (profile.timeDeltas[i] ?? 0));
}
const top = [...times].map(([id, us]) => ({ us, ...nodes.get(id)?.callFrame })).sort((a, b) => b.us - a.us).slice(0, 40);
const summary = { elapsedUs: profile.endTime - profile.startTime, samples: profile.samples?.length ?? 0, top };
await writeFile(file + '.summary.json', JSON.stringify(summary, null, 2));
console.log(JSON.stringify({ elapsedSeconds: summary.elapsedUs / 1e6, top: top.slice(0, 8) }));
