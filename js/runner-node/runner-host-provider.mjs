// M1.0 spike, host-provider topology: transpiled suite alone; the JS
// runner implements test-context (context.js) and drives tests directly.
import { tests } from "./suite/suite.js";
import { Context } from "./context.js";

const { all } = tests;

let passed = 0, failed = 0, skipped = 0;

const cases = await all();
for (const testCase of cases) {
  const name = testCase.name();
  console.log(`test ${name} ...`);

  const ctx = new Context((msg) => console.log(`    diag: ${msg}`));

  let verdict;
  try {
    await testCase.run(ctx);
    verdict = { status: "pass" };
  } catch (e) {
    const payload = e?.payload ?? e;
    if (payload?.tag === "failed") verdict = { status: "fail", detail: payload.val };
    else if (payload?.tag === "skipped") verdict = { status: "skip", detail: payload.val };
    else throw e;
  }

  switch (verdict.status) {
    case "pass": passed++; console.log(`test ${name}: PASS`); break;
    case "fail": failed++; console.log(`test ${name}: FAIL: ${verdict.detail}`); break;
    case "skip": skipped++; console.log(`test ${name}: SKIP: ${verdict.detail}`); break;
  }
}

console.log(`\nresult: ${passed} passed, ${failed} failed, ${skipped} skipped, ${cases.length} total`);
process.exit(failed === 0 ? 0 : 1);
