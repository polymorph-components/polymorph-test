# `lann:component-test`

Common infrastructure for testing WebAssembly components: a small WIT
contract between test **suites** (components that carry test cases) and test
**runners** (components or hosts that execute them), plus the tooling that
makes multi-target test operations tractable — inventory tracking, a
canonical results format, aggregation, and CI packaging.

Status: proposal. The WIT below is the seed; everything else is tracked in
the [issues](../../issues).

## The contract

[`wit/tests.wit`](wit/tests.wit) defines one interface and two worlds:

```wit
interface tests {
    variant outcome { pass, fail(string), skipped(string) }

    resource test-case {
        name: func() -> string;
        features: func() -> list<string>;
        run: async func() -> outcome;
    }

    all: func(missing-features: list<string>) -> list<test-case>;
}

world suite  { export tests; }
world runner { import tests; }
```

A suite exports `tests`; a runner imports it. Composition (e.g.
`wac plug --plug suite.wasm runner.wasm`) yields an executable artifact —
the linker is the test harness's registration step.

Design commitments, in decreasing order of load-bearing:

- **Cases are self-describing.** Each carries a stable hierarchical name and
  the feature names it exercises beyond the suite's baseline. Targets
  declare what they are *missing*; `all(missing-features)` materializes the
  suite accordingly. New cases run everywhere by default — coverage is shed
  only by explicit declaration.
- **Skips are claims, not absences.** `skipped(string)` says what the case
  asserted instead of exercising its subject (typically: that the missing
  feature is *declined*, not silently half-served).
- **`run` never traps.** Expectation mismatches are `fail`; a trap is
  recorded as that case's failure and the instance treated as poisoned.
- **Unknown feature names trap.** A misspelled declaration is a harness
  bug, not a test outcome.
- **The outcome variant is closed.** Variant cases in return position have
  no compatible growth path, so `outcome` is designed never to need one:
  three cases, details ride the string payloads. Metadata (timing,
  diagnostics) arrives as additive interfaces, never as outcome cases.
- **Expected failure is not on the guest surface.** Capability gaps are
  target facts (a manifest); known-not-yet-passing cases are runner-side
  ratchets. Bugs get fixed, not declared.

## Provenance

Synthesized from two sources:

- [`lann/wasi-test`](https://github.com/lann/wasi-test) — the composition
  model: suite exports, runner imports, `wac plug` links them.
- [`lann/webcrypto`](https://github.com/lann/webcrypto)'s conformance
  system — the operational model, hardened at ~8000-case scale:
  self-describing inventories, capability manifests, lockfiles, one results
  wire format, many adapters, one aggregator.

## Scope (tracked in issues)

- Guest SDKs: Rust (`suite!` macro) and JS (componentize-js).
- Runners: `wasi:cli`, `wasi:http` (served UI + remote API), in-browser via
  jco, native embedding with a libtest-mimic frontend.
- Inventory lockfiles and the update workflow.
- Canonical results JSON, aggregator/validator, markdown matrix, static
  viewer.
- Interop emitters: JUnit XML, TAP, GitHub Actions annotations.
- Reusable GitHub Actions workflows.
- A `component-test` CLI wrapping composition, execution, and aggregation.
