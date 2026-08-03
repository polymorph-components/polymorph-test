# Architecture

The features of this repo form layers. Each layer depends only on the layers
below it, and each boundary is a concrete artifact format — WIT, wasm
components, or JSON — so any layer can be replaced without disturbing its
neighbors.

```
L6  Workflow          reusable GitHub Actions; `component-test` CLI orchestration
L5  Reporting         markdown matrix, static viewer; interop emitters (JUnit, TAP, GHA annotations)
L4  Results           canonical results JSON; aggregator/validator
L3  Execution         runners: wasi:cli, wasi:http, jco/browser, native libtest-mimic embedding
L2  Composition       context provider component; provider → suite → core linking
L1  Contract          wit/tests.wit: test-context, tests, suite/runner worlds   ← the narrow waist
L0  Producers         guest SDKs (Rust `suite!`, JS); future: cases-as-exports + synthesizer (#18)
──  Cross-cutting     inventory & lockfiles; capability manifests; results schema versioning
```

## The layers

### L0 — Producers

Anything that yields an L1-conforming suite component: today, guest SDKs (a
Rust `suite!` macro, JS via componentize-js); in the future, alternative
authoring formats like the cases-as-exports format sketched in
[#18](../../issues/18), where a synthesizer acts as an L0→L1 compiler.

"Producers" rather than "authoring": humans author here, but so do code
generators and synthesizers. The defining property is what comes out the
top — a component that satisfies the `suite` world — not who or what made
it. New producer formats never touch anything above L1.

### L1 — Contract ("the narrow waist")

[`wit/tests.wit`](wit/tests.wit): the `test-context` and `tests` interfaces
and the `suite`/`runner` worlds. Everything above and below meets here, and
it is the only layer whose changes have semver-major consequences — which is
why the design budget was spent on it (see the design commitments in the
[README](README.md)). Layers L0–L3 exchange *wasm components typed by L1*;
layers L4 and up exchange *results JSON*. Those two interchange formats are
the repo's stable surfaces; everything else is implementation.

### L2 — Composition

The shared context-provider component (exporting `test-context`) and the
linking topology, which factors into two steps: **bundle** suite +
provider (a `wac` script re-exporting `tests`, `test-context`, and
`factory` from one shared provider instance), then **`wac plug`** the
bundle into a runner core. This is where "the linker is the test
harness's registration step" lives. Owned by the `component-test` CLI's
composition step but usable standalone.

### L3 — Execution

Runners: adapters from "execute an L1 suite over transport X" to "emit
canonical results JSON". Planned: `wasi:cli`, `wasi:http` (served UI +
remote API), in-browser via jco, native embedding with a libtest-mimic
frontend. Runners own execution policy — sequencing, timeouts,
re-instantiation after poisoning, known-failure ratchets — none of which
appears in L1.

### L4 — Results

One canonical results JSON schema, an aggregator, and a validator. The
operational lesson inherited from the webcrypto conformance system: many
adapters, one wire format, one aggregator. The L3/L4 boundary is the schema;
runners in any language on any host are interchangeable as long as they emit
it.

### L5 — Reporting

Consumers of aggregated results: markdown result matrix, static viewer, and
interop emitters (JUnit XML, TAP, GitHub Actions annotations). Emitters are
deliberately downstream of L4 rather than built into runners, so each
format is written once, not once per runner.

### L6 — Workflow

Reusable GitHub Actions workflows and the `component-test` CLI, which
orchestrates the stack end to end: compose (L2), execute (L3), aggregate
(L4), report (L5). The CLI is a convenience shell over the lower layers,
never the only way to reach them.

## Cross-cutting concerns

Not layers — they thread through several:

- **Inventory & lockfiles.** Case inventory is read from L1 today (via
  `all()`) and potentially from L0 statically in the future (#18's
  headline benefit), and consumed at L4 (aggregation completeness checks)
  and L6 (lockfile update workflow). Lockfiles pin case names *and*
  feature marks, and enforce the decline-pair lint (every positively
  marked feature has a `!feature` case). Depends on L1's determinism and
  name-stability guarantees.
- **Feature marks & capability manifests.** Marks (`<feature>` /
  `!<feature>`) are static L0 metadata (SDK-emitted custom section +
  lockfile); manifests are per-target facts (implementation ×
  environment). Applicability is a pure predicate over the two, evaluated
  at L3/L6 without executing anything — `run` is feature-blind, and cases
  excluded this way are reported `not-applicable` at L4. Structural
  features (world imports) gate whole suites at L2/L6, not cases.
- **Results schema versioning.** The L4 schema is the second frozen surface
  and needs its own evolution policy (additive fields only, explicit
  version tag) — tracked with the schema itself, but constraining L3–L6.
  Status vocabulary: `pass | fail | skipped` (executed) plus
  `not-applicable` (scheduler); `deselected` reserved for user-driven
  selection.

## Rules

1. **Dependency direction.** A layer may depend only on layers below it.
   Interchange happens at the two stable boundaries: L1 components below,
   L4 JSON above.
2. **Stability gradient.** L1 is frozen (semver-major to change). The L4
   schema is stable once published (additive evolution only). Everything
   else may churn freely.
3. **Named layer violation.** #18's "introspecting host runner" — L3
   reaching directly into L0, skipping L1/L2 — is permitted as a host
   optimization. It must remain behaviorally equivalent to the layered
   path and must never become a second contract: if an optimization needs
   something the L1 path can't express, that's an L1 design problem, not a
   license to extend the side channel.
