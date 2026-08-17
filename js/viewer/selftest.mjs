// Node selftest for the viewer's engines — the drift gate CI runs
// (`just verify-viewer`):
//
// 1. Aggregation: the wasm-compiled viewer-aggregate COMPONENT, run
//    under deltic exactly as the page runs it (js/viewer/deltic.mjs),
//    must reproduce the CLI gate's verdicts over the fixture pipeline
//    (same summary accounting, same expected-fail assessments).
// 2. Gating-adapter options of the shared harness loop (#50):
//    freshCases + caseTimeoutMs over synthetic cases.
//
// The suite-execution half of the old selftest lives in
// js/runner-deltic/selftest.mjs now — same harness.mjs loop, same
// suites, deltic engine (verify-deltic's last leg). Plain `node`:
// nothing here needs --experimental-wasm-jspi.
//
// Usage: node selftest.mjs <tests.lock> <targets.toml> <native.jsonl>
//   <sim.jsonl> <deltic-embedder.mjs> <translator.wasm>
//   <viewer-aggregate.wasm>
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { mergeCounts, runCases } from "./harness.mjs";
import { Context } from "./context.js";

const [lockPath, manifestPath, nativePath, simPath, bundlePath, translatorPath, aggregatePath] =
  process.argv.slice(2);
const fail = (msg) => {
  console.error(`selftest: ${msg}`);
  process.exit(1);
};
if (!aggregatePath) {
  fail(
    "usage: selftest.mjs <tests.lock> <targets.toml> <native.jsonl> " +
      "<sim.jsonl> <deltic-embedder.mjs> <translator.wasm> <viewer-aggregate.wasm>",
  );
}

// --- 1. Aggregation parity with the gate ---------------------------
// The same instantiation deltic.mjs performs in-page, with fs reads.
const deltic = await import(pathToFileURL(bundlePath).href);
const translator = await deltic.Translator.create(
  new Uint8Array(readFileSync(translatorPath)),
);
const componentBytes = new Uint8Array(readFileSync(aggregatePath));
const { plan, adapters } = translator.translate(componentBytes);
const inst = await deltic.instantiate(
  { plan, componentBytes, adapters },
  deltic.wasi(),
);
const aggregate = inst.exports.run;

const doc = JSON.parse(
  await aggregate(readFileSync(lockPath, "utf8"), readFileSync(manifestPath, "utf8"), [
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

// --- 2. Gating-adapter options (#50): freshCases + caseTimeoutMs ----
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
  // mergeCounts stays exercised here (the striped suite legs live in
  // js/runner-deltic/selftest.mjs).
  const twice = mergeCounts([counts, counts]);
  if (twice.total !== 6) fail(`mergeCounts: ${JSON.stringify(twice)}`);
}

console.log(
  `viewer selftest ok: aggregate ${JSON.stringify(doc.summary)} (deltic-linked, no JSPI flag)`,
);
