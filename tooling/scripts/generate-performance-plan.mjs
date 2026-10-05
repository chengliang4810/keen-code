/** 固定大小的合成目录和原生 UI 动作，前后构建使用同一计划摘要。 */
import { readFile, writeFile } from 'node:fs/promises';
const base = JSON.parse(await readFile('tooling/native-live/session-reconnect-plan.json', 'utf8'));
const editor = JSON.parse(await readFile('tooling/native-live/native-editor-plan.json', 'utf8'));
const files = { 'evidence.txt': 'NATIVE_PERFORMANCE_FILE_OK_41\n', 'preview.rs': Array.from({ length: 1500 }, (_, i) => `pub fn sample_${i}() -> usize { ${i} }`).join('\n') };
for (let d = 0; d < 400; d++) for (let f = 0; f < 50; f++) {
  files[`source-${String(d).padStart(3, '0')}/module-${String(f).padStart(3, '0')}.rs`] = `// synthetic ${d} ${f}\n`;
}
const wait = (expression, timeoutMs = 60000) => ({ action: 'wait', expression, timeoutMs });
const measure = (name, durationMs = 10000) => ({ action: 'measure', name, durationMs, settleMs: 2000 });
const runtime = (name) => ({ action: 'runtimeEvidence', name });
const search = '[data-testid="workspace-file-tree-panel"] input[type="text"]';
const steps = [...base.steps.slice(0, 14), measure('idle-start'), runtime('idle-start'),
  ...editor.steps.slice(8, 14), { action: 'cpuProfile', phase: 'start' },
  { action: 'type', selector: search, text: 'module-049' },
  wait("document.querySelector('[data-testid=\"workspace-file-tree-panel\"]')?.innerText.includes('module-049.rs')"),
  { action: 'cpuProfile', phase: 'stop', name: 'file-search' }, runtime('file-search'), measure('search-retained'),
  { action: 'clear', selector: search },
  { action: 'type', selector: search, text: 'preview.rs' },
  wait("!!document.querySelector('[data-testid^=\"workspace-file-tree-row-\"][data-testid$=\"preview.rs\"]')"),
  { action: 'click', selector: '[data-testid^="workspace-file-tree-row-"][data-testid$="preview.rs"]' },
  wait("!!document.querySelector('[data-testid=\"preview-pane\"]')"),
  runtime('code-preview'), measure('code-preview'), { action: 'screenshot', name: 'code-preview' },
  { action: 'journalBaseline', name: 'performance-turn', eventType: 'turn_started', captureEventSequence: true },
  { action: 'type', selector: '[data-testid="v4-composer-input"]', text: '只完成性能验收。先使用 Read 读取 evidence.txt 并确认 NATIVE_PERFORMANCE_FILE_OK_41。不要调用其他工具。然后用 rust 和 typescript 两个代码围栏，各输出 100 行不同的有效函数，每行用汉字注释说明序号，不能省略。最后单独输出 NATIVE_PERFORMANCE_DONE_41。' },
  { action: 'cpuProfile', phase: 'start' },
  { action: 'click', selector: '[data-testid="v4-composer-send"]' },
  { action: 'journalWait', eventType: 'turn_started', newSinceBaseline: 'performance-turn', captureSessionAs: 'performance-session', timeoutMs: 90000 },
  { action: 'journalWait', eventType: 'tool_completed', requestToolName: 'Read', outcomeStatus: 'succeeded', outcomeIsError: false, newSinceBaseline: 'performance-turn', sessionEqualsCaptured: 'performance-session', timeoutMs: 120000 },
  measure('real-stream', 20000),
  { action: 'journalWait', eventType: 'turn_completed', newSinceBaseline: 'performance-turn', sessionEqualsCaptured: 'performance-session', timeoutMs: 240000 },
  wait("Array.from(document.querySelectorAll('[data-row-id]')).some(e => e.classList.contains('group/assistant-row') && e.innerText.includes('NATIVE_PERFORMANCE_DONE_41')) && !document.querySelector('[data-testid=\"v4-stop\"]')", 120000),
  { action: 'cpuProfile', phase: 'stop', name: 'real-stream' }, runtime('after-real-stream'),
  { action: 'heapEvidence', name: 'after-real-stream' }, measure('after-real-stream'),
  { action: 'screenshot', name: 'real-model-completed' },
];
for (let i = 0; i < 6; i++) steps.push(measure(`idle-history-${i}`, 30000));
steps.push(runtime('idle-history-end'), { action: 'heapEvidence', name: 'idle-history-end' });
await writeFile('out/native-live/performance-plan.json', JSON.stringify({ name: 'native-live-performance-20261005', providerId: base.providerId, model: base.model, files, steps }, null, 2));
console.log(JSON.stringify({ files: Object.keys(files).length, steps: steps.length }));
