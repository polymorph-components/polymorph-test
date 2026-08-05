// Node selftest for the viewer's two engines — the drift gate CI runs
// (`just verify-viewer`):
//
// 1. Aggregation: the wasm-compiled viewer-aggregate component must
//    reproduce the CLI gate's verdicts over the fixture pipeline
//    (same summary accounting, same expected-fail assessments).
// 2. Live harness: harness.mjs must run the transpiled suites to the
//    documented verdicts (sample: 1/1/1; fixture under --missing hsm:
//    the trap fails, the decline passes, hsm rows are not-applicable).
//
// Usage: node --experimental-wasm-jspi selftest.mjs \
//   <tests.lock> <targets.toml> <native.jsonl> <sim.jsonl>
import { readFileSync } from "node:fs";
import { inventoryLookup, runCases, mergeCounts } from "./harness.mjs";
import { Context } from "./context.js";
import { run as aggregate } from "./generated/viewer-aggregate.js";

const [lockPath, manifestPath, nativePath, simPath] = process.argv.slice(2);
const fail = (msg) => {
  console.error(`selftest: ${msg}`);
  process.exit(1);
};

// --- 1. Aggregation parity with the gate ---------------------------
const doc = JSON.parse(
  aggregate(readFileSync(lockPath, "utf8"), readFileSync(manifestPath, "utf8"), [
    ["native", readFileSync(nativePath, "utf8")],
    ["sim", readFileSync(simPath, "utf8")],
  ]),
);
const expect = (cond, msg) => cond || fail(msg + `\n${JSON.stringify(doc.summary)}`);
expect(doc.summary.ok === true, "aggregate not ok");
expect(doc.summary.failing === 0, "failing != 0");
expect(doc.summary["expected-fail"] === 2, "expected-fail != 2");
expect(doc.errors.length === 0, `errors: ${doc.errors}`);
expect(
  doc.assessments.native?.["fixture/trap/boom"]?.kind === "expected-fail",
  "boom not assessed expected-fail",
);
expect(
  doc.results.sim?.["fixture/hsm/attest"]?.status === "not-applicable",
  "hsm/attest not scheduled out on sim",
);

// --- 2. Live harness over the transpiled suites --------------------
async function runSuite(name, missing) {
  const cores = [];
  for (const core of [`${name}.core.wasm`, `${name}.core2.wasm`]) {
    try {
      cores.push(new Uint8Array(readFileSync(new URL(`./suite/${core}`, import.meta.url))));
    } catch {
      continue;
    }
  }
  const tagsOf = inventoryLookup(cores);
  const { tests } = await import(`./suite/${name}.js`);
  const events = [];
  const counts = await runCases({
    cases: await tests.all(),
    Context,
    tagsOf,
    missing,
    emit: (event, index) => events.push({ index, event }),
  });
  return { counts, events };
}

const sample = await runSuite("sample-suite", []);
if (
  sample.counts.passed !== 1 ||
  sample.counts.failed !== 1 ||
  sample.counts.skipped !== 1
) {
  fail(`sample suite verdicts: ${JSON.stringify(sample.counts)}`);
}

const fixture = await runSuite("fixture-suite", ["hsm"]);
const byName = new Map(fixture.events.map(({ event }) => [event.case, event]));
if (byName.get("fixture/trap/boom")?.provenance !== "trap") {
  fail(`boom not a trap: ${JSON.stringify(byName.get("fixture/trap/boom"))}`);
}
if (byName.get("fixture/hsm/attest")?.status !== "not-applicable") {
  fail(`hsm/attest not scheduled out: ${JSON.stringify(byName.get("fixture/hsm/attest"))}`);
}
if (byName.get("fixture/hsm/declined")?.status !== "pass") {
  fail(`!hsm decline did not run/pass: ${JSON.stringify(byName.get("fixture/hsm/declined"))}`);
}

// Striping partitions: two shards reproduce the unsharded counts.
async function runSharded(name, missing, count) {
  const cores = [new Uint8Array(readFileSync(new URL(`./suite/${name}.core.wasm`, import.meta.url)))];
  const tagsOf = inventoryLookup(cores);
  const { tests } = await import(`./suite/${name}.js`);
  const parts = [];
  for (let index = 0; index < count; index++) {
    parts.push(
      await runCases({
        cases: await tests.all(),
        Context,
        tagsOf,
        missing,
        shard: { index, count },
        emit: () => {},
      }),
    );
  }
  return mergeCounts(parts);
}
const merged = await runSharded("fixture-suite", ["hsm"], 2);
if (JSON.stringify(merged) !== JSON.stringify(fixture.counts)) {
  fail(`sharded counts diverge: ${JSON.stringify(merged)} vs ${JSON.stringify(fixture.counts)}`);
}

// --- 3. Gating-adapter options (#50): freshCases + caseTimeoutMs ----
// Synthetic cases (the loop only needs name()/run()): a hanging case
// must produce the limit-exceeded row and not stall the loop, every
// execution must re-enumerate through the factory, and a case
// vanishing on re-enumeration must throw (drift), not fail.
{
  const mkCase = (name, run) => ({ name: () => name, run });
  const template = [
    mkCase("synthetic/pass", async () => {}),
    mkCase("synthetic/hang", () => new Promise(() => {})),
    mkCase("synthetic/fail", async () => {
      throw { payload: { tag: "failed", val: "boom" } };
    }),
  ];
  let enumerations = 0;
  const events = [];
  const counts = await runCases({
    cases: template,
    Context,
    tagsOf: () => [],
    missing: [],
    emit: (event) => events.push(event),
    caseTimeoutMs: 100,
    freshCases: async () => {
      enumerations++;
      return template;
    },
  });
  if (counts.passed !== 1 || counts.failed !== 2 || counts.total !== 3) {
    fail(`synthetic counts: ${JSON.stringify(counts)}`);
  }
  if (enumerations !== 3) {
    fail(`freshCases enumerated ${enumerations} times, want one per case`);
  }
  const hang = events.find((e) => e.case === "synthetic/hang");
  if (
    hang?.status !== "fail" ||
    hang?.provenance?.["limit-exceeded"] !== "case-timeout" ||
    hang?.["diagnostics-complete"] !== false
  ) {
    fail(`hang row: ${JSON.stringify(hang)}`);
  }
  const failed = events.find((e) => e.case === "synthetic/fail");
  if (failed?.provenance !== "returned" || failed?.detail !== "boom") {
    fail(`payload mapping under the race: ${JSON.stringify(failed)}`);
  }
  let vanished = false;
  try {
    await runCases({
      cases: [mkCase("synthetic/pass", async () => {})],
      Context,
      tagsOf: () => [],
      missing: [],
      emit: () => {},
      freshCases: async () => [],
    });
  } catch {
    vanished = true;
  }
  if (!vanished) {
    fail("vanished case on re-enumeration did not throw");
  }
}

console.log(
  `viewer selftest ok: aggregate ${JSON.stringify(doc.summary)}; ` +
    `sample ${JSON.stringify(sample.counts)}; fixture ${JSON.stringify(fixture.counts)}`,
);
