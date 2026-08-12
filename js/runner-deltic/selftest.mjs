// The deltic engines' drift gate (verify-deltic's selftest leg) — the
// runtime-linked sibling of js/viewer/selftest.mjs, running the SAME
// harness.mjs case loop over deltic-instantiated suites. Plain `node`,
// NO --experimental-wasm-jspi: deltic's callback-ABI path needs no
// engine flag, which is the browser-leg premise this gate pins on every
// PR (the real-browser proof lives in deltic's own post-merge lanes).
//
//   node js/runner-deltic/selftest.mjs <deltic-embedder.mjs> \
//     <deltic-translator-shim.wasm> <sample_suite.wasm> <fixture_suite.wasm>

import { strict as assert } from "node:assert";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { mergeCounts, runCases } from "../viewer/harness.mjs";
import { loadSuite } from "./engine.mjs";

const [bundlePath, translatorPath, samplePath, fixturePath] = process.argv.slice(2);
if (!fixturePath) {
  console.error(
    "usage: node js/runner-deltic/selftest.mjs <deltic-embedder.mjs> " +
      "<translator.wasm> <sample_suite.wasm> <fixture_suite.wasm>",
  );
  process.exit(2);
}

const bundle = await import(pathToFileURL(bundlePath).href);
const translatorBytes = new Uint8Array(readFileSync(translatorPath));

async function suiteOf(path, env) {
  return await loadSuite({
    bundle,
    translatorBytes,
    suiteBytes: new Uint8Array(readFileSync(path)),
    env,
  });
}

async function run(engine, { missing = [], only, shard } = {}) {
  const events = [];
  const counts = await runCases({
    cases: await (await engine.newTests()).all(),
    Context: engine.Context,
    tagsOf: engine.tagsOf,
    missing,
    only,
    shard,
    emit: (event, index) => events.push({ index, event }),
    freshCases: async () => (await engine.newTests()).all(),
  });
  events.sort((a, b) => a.index - b.index);
  return { counts, events: events.map((e) => e.event) };
}

// --- sample: the documented verdicts, no flags anywhere -----------------------
{
  const engine = await suiteOf(samplePath);
  const { counts, events } = await run(engine);
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

// --- fixture: trap containment + tag scheduling through the deltic engine -----
{
  const engine = await suiteOf(fixturePath);
  const { counts, events } = await run(engine, { missing: ["hsm"] });
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
  });
  assert.equal(byCase["fixture/hsm/declined"].status, "pass");
  console.log("selftest: fixture trap + tag scheduling ok");

  // Selection (#89): the unselected census is reported deselected —
  // full coverage, never executed, capability winning over selection
  // (hsm/attest stays not-applicable outside the filter, exactly the
  // reference runner's precedence). The trap case is outside the
  // selection: nothing fails.
  const sub = await run(engine, { missing: ["hsm"], only: "gen" });
  assert.deepEqual(sub.counts, {
    passed: 2,
    failed: 0,
    skipped: 0,
    na: 1,
    deselected: 5,
    selected: 2,
    total: 8,
  });
  const subByCase = Object.fromEntries(sub.events.map((e) => [e.case, e]));
  assert.deepEqual(subByCase["fixture/trap/boom"], {
    case: "fixture/trap/boom",
    status: "deselected",
    detail: "only gen",
  });
  assert.equal(subByCase["fixture/hsm/attest"].status, "not-applicable");
  assert.equal(subByCase["fixture/gen/tc1"].status, "pass");
  assert.equal(sub.events.length, 8, "full census reported");
  // A selection matching nothing is a run error, not a vacuous green.
  await assert.rejects(
    () => run(engine, { only: "zzz" }),
    /empty selection is a run error/,
  );
  console.log("selftest: only -> deselected census ok");

  // Striping partition equality (harness semantics over the deltic engine):
  // two shards merge to the full counts, disjoint cases, full union.
  const s0 = await run(engine, { missing: ["hsm"], shard: { index: 0, count: 2 } });
  const s1 = await run(engine, { missing: ["hsm"], shard: { index: 1, count: 2 } });
  assert.deepEqual(mergeCounts([s0.counts, s1.counts]), counts);
  const names = (r) => r.events.map((e) => e.case);
  const union = new Set([...names(s0), ...names(s1)]);
  assert.equal(union.size, names(s0).length + names(s1).length, "disjoint shards");
  assert.deepEqual([...union].sort(), events.map((e) => e.case).sort());
  console.log("selftest: striping partition equality ok");
}

console.log("selftest: deltic engines ok");
