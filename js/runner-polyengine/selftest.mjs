// The polyengine engine's drift gate (verify-polyengine's selftest leg) — the
// runtime-linked sibling of js/viewer/selftest.mjs, driving the SAME
// ct-runner `runSuite` case loop the browser worker (./worker-main.mjs) and
// the Deno leg (./runner.ts) use. Plain `node`, NO
// --experimental-wasm-jspi: polyengine's callback-ABI path needs no engine
// flag, which is the browser-leg premise this gate pins on every PR (the
// real-browser proof lives in polyengine's own post-merge lanes).
//
//   node js/runner-polyengine/selftest.mjs <polyengine-embedder.mjs> \
//     <polyengine-translator-shim.wasm> <sample_suite.wasm> <fixture_suite.wasm>

import { strict as assert } from "node:assert";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { mergeCounts } from "../viewer/harness.mjs";

const [bundlePath, translatorPath, samplePath, fixturePath] = process.argv.slice(2);
if (!fixturePath) {
  console.error(
    "usage: node js/runner-polyengine/selftest.mjs <polyengine-embedder.mjs> " +
      "<translator.wasm> <sample_suite.wasm> <fixture_suite.wasm>",
  );
  process.exit(2);
}

const polyengine = await import(pathToFileURL(bundlePath).href);
const translator = await polyengine.Translator.create(
  new Uint8Array(readFileSync(translatorPath)),
);

function artifactsOf(path) {
  const suiteBytes = new Uint8Array(readFileSync(path));
  const { plan, adapters } = translator.translate(suiteBytes);
  return { plan, componentBytes: suiteBytes, adapters };
}

async function run(artifacts, { missing, only, shard } = {}) {
  const events = [];
  const counts = await polyengine.runSuite(artifacts, {
    imports: polyengine.wasi({ cli: { env: {} } }),
    target: "polyengine/node-selftest",
    suiteName: "selftest",
    missing,
    only,
    shard,
    emit: (line, index) => {
      // Envelope and terminator carry no case index — the worker drops them
      // the same way (./worker-main.mjs).
      if (index !== undefined) events.push({ index, event: JSON.parse(line) });
    },
  });
  events.sort((a, b) => a.index - b.index);
  // ./worker-main.mjs's counts derivation, verbatim: `RunCounts.total` is
  // this stripe's pre-`only` census size, and the parent page-runner needs
  // `selected` for its empty-selection check.
  const selected = counts.passed + counts.failed + counts.skipped + counts.na;
  return {
    counts: { ...counts, selected, deselected: counts.total - selected },
    events: events.map((e) => e.event),
  };
}

// --- sample: the documented verdicts, no flags anywhere -----------------------
{
  const { counts, events } = await run(artifactsOf(samplePath));
  assert.deepEqual(counts, {
    passed: 1,
    failed: 1,
    skipped: 1,
    na: 0,
    deselected: 0,
    selected: 3,
    total: 3,
  });
  const byCase = Object.fromEntries(events.map((e) => [e.case, e]));
  assert.equal(byCase["sample/math/add"].status, "pass");
  assert.equal(byCase["sample/math/mul"].status, "fail");
  assert.equal(byCase["sample/token/attest"].status, "skipped");
  console.log("selftest: sample verdicts ok (callback ABI, no JSPI flag)");
}

// --- fixture: trap containment + tag scheduling through the polyengine engine -----
{
  const fixture = artifactsOf(fixturePath);
  const { counts, events } = await run(fixture, { missing: ["hsm"] });
  assert.deepEqual(counts, {
    passed: 6,
    failed: 1,
    skipped: 0,
    na: 1,
    deselected: 0,
    selected: 8,
    total: 8,
  });
  const byCase = Object.fromEntries(events.map((e) => [e.case, e]));
  assert.equal(byCase["fixture/trap/boom"].status, "fail");
  assert.equal(byCase["fixture/trap/boom"].provenance, "trap");
  assert.equal(byCase["fixture/trap/boom"]["diagnostics-complete"], false);
  // The case AFTER the trap runs green: freshCases containment.
  assert.equal(byCase["fixture/trap/after"].status, "pass");
  assert.deepEqual(byCase["fixture/hsm/attest"], {
    case: "fixture/hsm/attest",
    status: "not-applicable",
    detail: "hsm",
    "diagnostics-complete": true,
  });
  assert.equal(byCase["fixture/hsm/declined"].status, "pass");
  console.log("selftest: fixture trap + tag scheduling ok");

  // Selection (#89): `runSuite` skips non-matching cases with no emit, so
  // the deselected census is a count, not rows (docs/runner-policy.md's
  // "selection is not capability" still holds — capability wins over
  // selection for whatever the filter admits). The trap case is outside the
  // selection: nothing fails.
  const sub = await run(fixture, { missing: ["hsm"], only: "gen" });
  assert.deepEqual(sub.counts, {
    passed: 2,
    failed: 0,
    skipped: 0,
    na: 0,
    deselected: 6,
    selected: 2,
    total: 8,
  });
  const subByCase = Object.fromEntries(sub.events.map((e) => [e.case, e]));
  assert.equal(subByCase["fixture/trap/boom"], undefined, "deselected: no row");
  assert.equal(subByCase["fixture/gen/tc1"].status, "pass");
  assert.equal(sub.events.length, 2, "only the selected cases are reported");
  // A selection matching nothing is a vacuous run here; the PARENT
  // (js/viewer/page-runner.mjs) turns `selected === 0` into the run error.
  const none = await run(fixture, { missing: ["hsm"], only: "zzz" });
  assert.equal(none.counts.selected, 0);
  assert.equal(none.events.length, 0);
  console.log("selftest: only -> selection counts ok");

  // Striping partition equality (runSuite semantics over the polyengine
  // engine): two shards merge to the full counts, disjoint cases, full union.
  const s0 = await run(fixture, { missing: ["hsm"], shard: { index: 0, count: 2 } });
  const s1 = await run(fixture, { missing: ["hsm"], shard: { index: 1, count: 2 } });
  assert.deepEqual(mergeCounts([s0.counts, s1.counts]), counts);
  const names = (r) => r.events.map((e) => e.case);
  const union = new Set([...names(s0), ...names(s1)]);
  assert.equal(union.size, names(s0).length + names(s1).length, "disjoint shards");
  assert.deepEqual([...union].sort(), events.map((e) => e.case).sort());
  console.log("selftest: striping partition equality ok");
}

console.log("selftest: polyengine engine ok");
