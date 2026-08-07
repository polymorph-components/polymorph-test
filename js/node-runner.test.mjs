// Unit checks for the node driver helpers (js/node-runner.mjs) and the
// shared suite-runner loop (runSuiteJsonl). Plain node, no transpiled
// suites: cases and instances are stubs. `just verify-imports`.

import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { runSuiteJsonl } from "./viewer/harness.mjs";
import { loadCoreModules, resolveTestsExport, writeResultsFile } from "./node-runner.mjs";

// loadCoreModules: prefix filtering, name order, non-wasm noise
// ignored, empty is an error. The 8-byte header is a valid (empty)
// core module, so WebAssembly.compile accepts it.
const EMPTY_MODULE = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
const dir = await mkdtemp(join(tmpdir(), "node-runner-test-"));
try {
  for (const name of ["a.core.wasm", "a.core2.wasm", "b.core.wasm"]) {
    await writeFile(join(dir, name), EMPTY_MODULE);
  }
  await writeFile(join(dir, "a.js"), "// not wasm");

  const a = await loadCoreModules(dir, "a");
  assert.deepEqual([...a.modules.keys()], ["a.core.wasm", "a.core2.wasm"]);
  assert.equal(a.coreBytes.length, 2);
  assert.ok(a.modules.get("a.core.wasm") instanceof WebAssembly.Module);

  const all = await loadCoreModules(dir);
  assert.equal(all.modules.size, 3, "no prefix loads every wasm");

  await assert.rejects(() => loadCoreModules(dir, "zzz"), /zzz\.core\*\.wasm under/);
} finally {
  await rm(dir, { recursive: true, force: true });
}

// resolveTestsExport: every spelling, and the error names the exports.
const tests = { all: async () => [] };
assert.equal(resolveTestsExport({ tests }), tests);
assert.equal(resolveTestsExport({ "polymorph:test/tests@0.1.0": tests }), tests);
assert.equal(resolveTestsExport({ "polymorph:test/tests": tests }), tests);
assert.throws(() => resolveTestsExport({ other: 1 }), /no tests interface: other/);

// runSuiteJsonl: envelope first (name normalized), one line per case,
// terminator last; scheduling against missing; fresh instances per
// case; counts returned; zero cases is an error.
const stubCase = (name, body) => ({ name: () => name, run: body ?? (async () => {}) });
let instances = 0;
const newTests = async () => {
  instances += 1;
  return {
    all: async () => [
      stubCase("basic/pass"),
      stubCase("basic/fail", async () => {
        throw { payload: { tag: "failed", val: "boom" } };
      }),
      stubCase("gated/probe"),
    ],
  };
};
const tags = { "basic/pass": [], "basic/fail": [], "gated/probe": ["hsm"] };
const lines = [];
const counts = await runSuiteJsonl({
  newTests,
  tagsOf: (name) => tags[name],
  target: "stub-target",
  suiteName: "sample-suite",
  missing: ["hsm"],
  emit: (line) => lines.push(line),
});
assert.deepEqual(counts, { passed: 1, failed: 1, skipped: 0, na: 1, total: 3 });
assert.equal(lines.length, 5, "envelope + three events + terminator");
const head = JSON.parse(lines[0]);
assert.equal(head.suite.name, "sample_suite", "envelope normalizes the transpile name");
assert.equal(head.target, "stub-target");
assert.equal(lines.at(-1), '{"segment-end":true}');
const events = lines.slice(1, -1).map((l) => JSON.parse(l));
assert.deepEqual(
  events.map((e) => e.status),
  ["pass", "fail", "not-applicable"],
);
assert.equal(events[1].detail, "boom");
// census + one fresh instance per executed case (the N/A case never runs)
assert.equal(instances, 3);

await assert.rejects(
  () =>
    runSuiteJsonl({
      newTests: async () => ({ all: async () => [] }),
      tagsOf: () => [],
      target: "t",
      suiteName: "s",
      emit: () => {},
    }),
  /empty selection is a run error/,
);

// writeResultsFile: creates the dir, returns the path, trailing newline.
const outDir = await mkdtemp(join(tmpdir(), "node-runner-out-"));
try {
  const path = await writeResultsFile({
    dir: join(outDir, "nested"),
    target: "stub-target",
    lines: ["a", "b"],
  });
  const { readFile: rf } = await import("node:fs/promises");
  assert.equal(await rf(path, "utf8"), "a\nb\n");
  assert.ok(path.endsWith("stub-target.jsonl"));
} finally {
  await rm(outDir, { recursive: true, force: true });
}

console.log("node-runner selftest OK");
