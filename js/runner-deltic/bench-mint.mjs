// Synthetic handle-mint benchmark for the deltic leg — the runtime-linked
// sibling of crates/component-test-runner/src/bin/bench-mint.rs, measuring
// the same phases per fresh instance under the pinned deltic embedder on
// plain Node (callback ABI, no engine flags):
//
//   instantiate  deltic.instantiate (runtime link + wasm instantiation)
//   all#1        first all(): guest registry build + mint + lift (N wrappers)
//   all#2        second all(): mint + lift only (guest registry cached)
//   name[0]      one test-case.name boundary call
//   run[0]       one trivial case execution (borrowed context)
//
// No teardown phase: deltic instances are GC-reclaimed, there is no
// dispose surface. Case count rides the BENCH_CASES wasi env import
// (see components/bench-suite).
//
//   node js/runner-deltic/bench-mint.mjs <deltic-embedder.mjs> \
//     <deltic-translator-shim.wasm> <bench_suite.wasm> \
//     [--cases 100,1000,10000] [--instances 10]

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const args = process.argv.slice(2);
const positional = [];
let caseCounts = [100, 1000, 10000];
let instances = 10;
for (let i = 0; i < args.length; i++) {
  switch (args[i]) {
    case "--cases":
      caseCounts = args[++i].split(",").map((s) => Number(s));
      break;
    case "--instances":
      instances = Number(args[++i]);
      break;
    default:
      if (args[i].startsWith("--")) throw new Error(`unknown flag '${args[i]}'`);
      positional.push(args[i]);
  }
}
const [bundlePath, translatorPath, suitePath] = positional;
if (!suitePath) {
  console.error(
    "usage: node bench-mint.mjs <deltic-embedder.mjs> <translator.wasm> " +
      "<bench_suite.wasm> [--cases N,N,...] [--instances M]",
  );
  process.exit(2);
}

const deltic = await import(pathToFileURL(bundlePath).href);
const suiteBytes = new Uint8Array(readFileSync(suitePath));

let t = performance.now();
const translator = await deltic.Translator.create(
  new Uint8Array(readFileSync(translatorPath)),
);
const translatorMs = performance.now() - t;

t = performance.now();
const { plan, adapters } = translator.translate(suiteBytes);
const translateMs = performance.now() - t;
console.error(
  `translator init: ${translatorMs.toFixed(1)}ms  ` +
    `translate: ${translateMs.toFixed(1)}ms  (${suiteBytes.length} bytes)`,
);

const artifacts = { plan, componentBytes: suiteBytes, adapters };
const TESTS = "polymorph:test/tests@0.1.0";

function median(xs) {
  const v = [...xs].sort((a, b) => a - b);
  return v[Math.floor(v.length / 2)];
}

const fmt = (ms) => (ms >= 1 ? `${ms.toFixed(2)}ms` : `${(ms * 1000).toFixed(1)}µs`);

console.log(
  "cases".padStart(6) +
    ["instantiate", "all#1", "all#2", "name[0]", "run[0]", "ns/handle"]
      .map((h) => h.padStart(12))
      .join(""),
);

for (const n of caseCounts) {
  const imports = {
    ...deltic.wasi({ cli: { env: { BENCH_CASES: String(n) } } }),
    ...deltic.testContextImportRecord(),
  };
  const warmup = Math.min(3, instances);
  const samples = { instantiate: [], all1: [], all2: [], name0: [], run0: [] };
  for (let i = 0; i < instances + warmup; i++) {
    let t = performance.now();
    const inst = await deltic.instantiate(artifacts, imports);
    const instantiate = performance.now() - t;
    const tests = inst.exports[TESTS] ?? inst.exports["tests"];

    t = performance.now();
    const cases1 = await tests.all();
    const all1 = performance.now() - t;
    if (cases1.length !== n) {
      throw new Error(`suite minted ${cases1.length} cases, expected ${n}`);
    }

    t = performance.now();
    const cases2 = await tests.all();
    const all2 = performance.now() - t;
    if (cases2.length !== n) throw new Error("second all() disagreed");

    t = performance.now();
    const name = String(await cases1[0].name());
    const name0 = performance.now() - t;
    if (!name.startsWith("bench/mint/")) {
      throw new Error(`unexpected case name '${name}'`);
    }

    const ctx = new deltic.Context(() => {});
    t = performance.now();
    await cases1[0].run(ctx); // resolves = pass; throws = fail/trap
    const run0 = performance.now() - t;

    if (i >= warmup) {
      samples.instantiate.push(instantiate);
      samples.all1.push(all1);
      samples.all2.push(all2);
      samples.name0.push(name0);
      samples.run0.push(run0);
    }
  }
  const all2 = median(samples.all2);
  console.log(
    String(n).padStart(6) +
      [
        fmt(median(samples.instantiate)),
        fmt(median(samples.all1)),
        fmt(all2),
        fmt(median(samples.name0)),
        fmt(median(samples.run0)),
        ((all2 * 1e6) / n).toFixed(0),
      ]
        .map((s) => s.padStart(12))
        .join(""),
  );
}
