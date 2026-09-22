export function percentile(sorted, fraction) {
  return sorted[Math.floor((sorted.length - 1) * fraction)];
}

export function summarizeBenchmark(values) {
  if (!values.length || values.some((value) => !Number.isFinite(value) || value < 0)) {
    throw new TypeError("benchmark samples must be finite non-negative numbers");
  }
  const sorted = [...values].sort((a, b) => a - b);
  const median = percentile(sorted, 0.5);
  const deviations = sorted.map((value) => Math.abs(value - median)).sort((a, b) => a - b);
  const p25 = percentile(sorted, 0.25);
  const p75 = percentile(sorted, 0.75);
  return {
    samples: sorted.length,
    medianMs: median,
    p25Ms: p25,
    p75Ms: p75,
    iqrMs: p75 - p25,
    madMs: percentile(deviations, 0.5),
    minMs: sorted[0],
    maxMs: sorted.at(-1),
  };
}
