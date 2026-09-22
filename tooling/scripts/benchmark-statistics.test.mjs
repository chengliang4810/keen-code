import assert from "node:assert/strict";
import test from "node:test";
import { summarizeBenchmark } from "./benchmark-statistics.mjs";

test("summarizeBenchmark reports robust startup statistics", () => {
  assert.deepEqual(summarizeBenchmark([40, 10, 30, 20, 50]), {
    samples: 5,
    medianMs: 30,
    p25Ms: 20,
    p75Ms: 40,
    iqrMs: 20,
    madMs: 10,
    minMs: 10,
    maxMs: 50,
  });
  assert.throws(() => summarizeBenchmark([]), /finite non-negative/);
});
