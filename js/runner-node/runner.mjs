// M1.0 spike: host-side JS runner driving the transpiled bundle on Node.
// The jco analog of the wasmtime host-embed runner: the runner lives
// outside the trap boundary; the bundle (suite + provider) is the wasm.
import * as tests from "./bundle/bundle.js";

const { all } = tests.tests;
const { newContext } = tests.factory;

let passed = 0, failed = 0, skipped = 0;

const cases = await all();
for (const testCase of cases) {
  const name = testCase.name();
  console.log(`test ${name} ...`);

  const [ctx, observer] = newContext();
  const diag = observer.diagnostics();

  // Drain diagnostics concurrently with the run.
  const drain = (async () => {
    const reader = diag.getReader();
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      console.log(`    diag: ${value}`);
    }
  })();

  let verdict; // { status, detail }
  try {
    try {
      await testCase.run(ctx);
      verdict = { status: "pass" };
    } catch (e) {
      // jco maps result::err to a thrown ComponentError with .payload
      const payload = e?.payload ?? e;
      if (payload?.tag === "failed") verdict = { status: "fail", detail: payload.val };
      else if (payload?.tag === "skipped") verdict = { status: "skip", detail: payload.val };
      else throw e; // trap or shim bug: not a case verdict
    }
  } finally {
    // Dispose and settle the drain on every path: an abandoned drain
    // promise rejecting after a trap would kill Node with an unhandled
    // rejection, masking the real error.
    if (ctx[Symbol.dispose]) ctx[Symbol.dispose]();
    await drain.catch(() => {});
    if (observer[Symbol.dispose]) observer[Symbol.dispose]();
  }

  switch (verdict.status) {
    case "pass": passed++; console.log(`test ${name}: PASS`); break;
    case "fail": failed++; console.log(`test ${name}: FAIL: ${verdict.detail}`); break;
    case "skip": skipped++; console.log(`test ${name}: SKIP: ${verdict.detail}`); break;
  }
}

console.log(`\nresult: ${passed} passed, ${failed} failed, ${skipped} skipped, ${cases.length} total`);
// Empty selection is a run error: zero cases must not exit green.
process.exit(failed === 0 && cases.length > 0 ? 0 : 1);
