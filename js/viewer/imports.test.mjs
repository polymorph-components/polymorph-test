// Unit checks for the shared consumer glue: the import-object builder
// (js/viewer/imports.mjs) and the envelope's suite-name normalization.
// Plain node, no wasm: `just verify-imports`.

import assert from "node:assert/strict";

import { envelope } from "./harness.mjs";
import { bindImports, envInterface } from "./imports.mjs";

// envelope: kebab-case package names normalize to the wasm-stem
// lockfile identity; already-normalized names pass through.
assert.equal(envelope("t", "conformance-guest-ct").suite.name, "conformance_guest_ct");
assert.equal(envelope("t", "conformance_guest_ct").suite.name, "conformance_guest_ct");

// bindImports: bare + versioned spellings, absent shim members skipped,
// explicit environment, SUT suffixing, test-context always present.
const exit = { exit: () => {} };
const streams = {};
const sutImpl = {};
const imports = bindImports({
  wasi: { cli: { exit }, io: { streams } },
  env: [["A", "1"]],
  sut: { "polymorph:websocket/connections": sutImpl },
  wasiVersions: ["0.2.0", "0.2.6"],
});

assert.equal(imports["wasi:cli/exit"], exit);
assert.equal(imports["wasi:cli/exit@0.2.0"], exit);
assert.equal(imports["wasi:cli/exit@0.2.6"], exit);
assert.equal(imports["wasi:io/streams@0.2.6"], streams);
assert.ok(!("wasi:clocks/monotonic-clock" in imports), "absent shim members are not bound");
assert.deepEqual(imports["wasi:cli/environment"].getEnvironment(), [["A", "1"]]);
assert.deepEqual(imports["wasi:cli/environment@0.2.6"].getArguments(), []);
assert.equal(imports["polymorph:websocket/connections"], sutImpl);
assert.equal(imports["polymorph:websocket/connections@0.1.0"], sutImpl);
assert.ok(imports["polymorph:test/test-context"].Context, "test-context provider bound");
assert.ok(imports["polymorph:test/test-context@0.1.0"].Context);

// envInterface: explicit list, no arguments, no cwd.
const env = envInterface([["B", "2"]]);
assert.deepEqual(env.getEnvironment(), [["B", "2"]]);
assert.equal(env.initialCwd(), undefined);

console.log("imports selftest OK");
