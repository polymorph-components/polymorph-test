# AGENTS.md

Orientation for coding agents (and other newcomers) working in this
repository. Read this before making changes; most expensive mistakes
here are edits to frozen surfaces or verification steps skipped.

## What this is

`polymorph:test`: common infrastructure for testing WebAssembly
components. A small frozen WIT contract between test *suites* and test
*runners*, plus the tooling around it: guest SDK, inventory lockfiles,
capability manifests via feature tags, a canonical results format,
runners (wasmtime host-embed, composed wasi:cli, jco/Node), and a CLI.

Design intent lives in three documents, in decreasing authority:

- `README.md` — the contract, the feature-tag scheme, and the design
  commitments (each is load-bearing; do not weaken one without explicit
  direction).
- `ARCHITECTURE.md` — the layer model (L0 producers … L6 workflow), the
  stability gradient, and the one sanctioned layer violation.
- `docs/findings.md` — empirical toolchain findings. When you discover
  new toolchain behavior (wasmtime/wit-bindgen/jco/wac), append it here.

Work is tracked in GitHub issues (`gh issue list -R polymorph-components/polymorph-test`).
Active milestone: **M1 — replace the webcrypto conformance harness**
(issues #27–#33). Design-history context is in issue threads; check
them before re-litigating a decision.

## Frozen surfaces — do not change casually

1. **`wit/tests.wit`** (L1). Semver-major to change. Nothing in return
   position can ever grow (no variant/record subtyping in the component
   model). Growth channels: new methods on the runner-implemented
   `context` resource; new optional suite-side interfaces. If a change
   here seems necessary, stop and surface it.
2. **The case-name grammar** (README "Case names", enforced in
   `crates/component-test-core/src/name.rs`). Segments `[a-z0-9._-]`,
   non-leaf segments must be WIT labels. Loosening is compatible;
   tightening never is.
3. **The results schema** (#26; `crates/component-test-formats/src/results.rs`).
   Additive evolution only; unknown statuses must remain tolerated.

## Layout

```
wit/                  L1 contract (the canonical copy)
crates/               host-side Rust (tested natively at root)
  component-test-core      name grammar, tags, verdicts
  component-test-formats   lockfile, inventory scanner, aggregator, matrix
  component-test-results   canonical results schema + fold (guest-linkable)
  component-test-sdk       guest SDK (registry, prelude, verdict ergonomics)
  component-test-sdk-macro #[suite] attribute (case discovery, tags section)
  component-test-cli       `component-test` bin: lock, fold, aggregate
  component-test-runner    wasmtime host-embed runner (`ct-runner` bin)
components/           guest components (build with --target wasm32-wasip2)
  provider                 reference context provider
  runner-cli               composed wasi:cli runner core
  sample-suite             demo suite (pass/fail/runtime-skip; #[suite] DX)
  fixture-suite            runner fixture (trap, tagged pair, generated row)
  drift-fixture            broken by design: raw registration, drift check
  zero-gen-fixture         broken by design: zero-row decline generator
  hang-fixture             broken by design: CPU spin + async wedge (#45)
js/runner-deltic/     deltic runner leg: CLI, browser shard worker, node
                      selftest (runner-is-provider topology; release-pinned)
actions/              composite GitHub Actions for consumers (#14):
                      aggregate (matrix -> job summary, findings ->
                      annotations, gate passthrough)
js/viewer/            results viewer + live browser harness (#46/#5): the
                      matrix page (aggregation = the gate's Rust compiled
                      to wasm) and the shared browser-safe runner core
                      (harness.mjs); gated by `just verify-viewer`
examples/compose/     wac composition walkthrough (bundle-then-plug)
expected/             golden outputs diffed by the verify recipes
docs/findings.md      toolchain findings log
```

**WIT deps are symlinks; suites have none.** `components/provider` and
`components/runner-cli` wit trees link back to the canonical copies
(`wit/` for the contract; `components/provider/wit/provider.wit` for
the provider WIT) — validate with `just wit-check`. Suite crates have
no wit dir at all: the SDK crate generates the contract bindings from
its own symlink (`crates/component-test-sdk/tests.wit` → `wit/tests.wit`),
and the `#[suite]` macro emits references to them. Don't replace links
with copies; symlinks require `core.symlinks` on Windows.

## Toolchain

wasmtime 47 (`-W component-model-async -S p3`), wac-cli 0.10, wit-bindgen
0.60, Node 24 (plain — the selftest legs; no engine flags anywhere:
deltic's callback ABI needs no JSPI), deno 2.9 (deltic runner
leg + browser-asset build; deltic itself is consumed from JSR as exact
prerelease pins — see `js/runner-deltic/README.md`), Rust
target `wasm32-wasip2`, wasm-tools (WIT validation), just (task
runner). jco is GONE from this repo (the deltic migration's Phase 4
cutover); its findings in `docs/findings.md` are historical. Known
sharp edges are catalogued in `docs/findings.md` — read
it before fighting the toolchain; your bug is probably finding #5, #6,
or #13.

## Build & verify

Recipes live in the `justfile` (`just` lists them): `just test` (host
crates), `just test-wasm` (integration tests that execute built
components — `#[ignore]`d in plain `cargo test`), `just build`
(components), `just all` (the full matrix below), `just lock-check` /
`just lock-update`, `just wit-check`, `just check` (the fast host gate:
fmt, clippy, host tests, WIT), `just ci` (exactly what CI's gating jobs
run, via the gha module). The verify
recipes assert exit codes *and* diff runner/fold output byte-for-byte
against `expected/` — when you intentionally change output or suite
cases, regenerate the affected `expected/` files and review the diff.
The underlying commands, for when you need them directly:

Host crates (fast, run this always; workspace `default-members`
excludes the wasm-only `components/*`):

```sh
cargo test
```

Guest components:

```sh
cargo build --target wasm32-wasip2 --release \
  -p sample-suite -p provider -p runner-cli -p fixture-suite
```

Full verification matrix (run whatever your change touches; all of
them before committing anything cross-cutting):

1. **Host-embed runner** (also exercises tags scheduling + trap path):
   ```sh
   cargo run -q -p component-test-runner --bin ct-runner -- \
     target/wasm32-wasip2/release/sample_suite.wasm
   cargo run -q -p component-test-runner --bin ct-runner -- \
     target/wasm32-wasip2/release/fixture_suite.wasm --missing hsm
   ```
   Expected: sample = 1 passed / 1 failed / 1 skipped; fixture = 6
   passed / 1 failed (trap) / 1 N/A / 8 total (incl. two generated
   cases and a depth-2 nested module). Exit code 1 in both.
2. **Composed runner** (see `examples/compose/README.md`): bundle via
   `wac compose`, plug via `wac plug`, run under `wasmtime run -W
   component-model-async -S p3`. Same sample-suite verdicts.
3. **deltic runner + browser leg** (see `js/runner-deltic/README.md`):
   drive the suite directly under deltic — no transpile step, no engine
   flag; JSR-pinned in `js/runner-deltic/deno.json` + `deno.lock`. Same
   sample verdicts (shared human + fold goldens); tag scheduling from the
   suite's own embedded inventory (fixture leg runs `--missing hsm` like
   Paths 1/4, lane goldens in `expected/verify-deltic-*`). The browser
   worker (`browser-worker.mjs`, drop-in for `page-runner.mjs` via
   `workerUrl`) shares `harness.mjs`'s case loop; `selftest.mjs` gates
   that engine path under plain node as `verify-deltic`'s last leg. (The
   jco-node runner that used to be this path was deleted in the deltic
   migration's Phase 4; `js/viewer/browser-worker.mjs` remains as
   consumer-facing glue for transpiled-module layouts.)
4. **Inventory + results pipeline**:
   ```sh
   cargo run -q -p component-test-cli -- lock \
     target/wasm32-wasip2/release/sample_suite.wasm \
     --check components/sample-suite/tests.lock
   cargo run -q -p component-test-runner --bin ct-runner -- \
     target/wasm32-wasip2/release/sample_suite.wasm --jsonl \
     | cargo run -q -p component-test-cli -- fold components/sample-suite/tests.lock
   ```

Lockfiles (`components/*/tests.lock`) are generated artifacts:
regenerate with `component-test lock ... -o` after any suite change and
commit the diff (the diff *is* the review surface). Never hand-edit.
The recorded `artifact-sha256` is provenance only — builds are not
reproducible across environments, so nothing may require it to match a
hash computed elsewhere (#44); the inventory is the binding.

## Conventions

- Rust 2021, workspace deps in the root `Cargo.toml`. `crates/` =
  host-side (must build and test natively); `components/` = guest-side
  (built for wasm32-wasip2 only; excluded via workspace
  `default-members` — a native `cargo test` on them proves nothing
  even where it happens to compile).
- One-line trap/fail details; diagnostics are the sideband for detail.
- Commit style: imperative summary line, body explains *why* and records
  findings. Verification results belong in the commit body when they
  motivated a change.
- No retries anywhere in test execution paths (masking flakes is worse
  than reporting them).
- When a design question arises mid-task: prefer filing/annotating an
  issue over silently deciding; the issue threads are the project's
  memory.

## Invariants easy to break by accident

- **Runners observing diagnostics must drain them concurrently with
  `run`** — a non-draining observer wedges sync-lifted suites (finding
  #3). Don't "simplify" the select loops in runners.
- **Drain diagnostics biased before taking the verdict**; dropping an
  in-flight stream read loses data (finding #5).
- **`context.diagnostic` stays `async func`** — sync is unimplementable
  guest-side (finding #1).
- **Tags/lockfile generation reads the suite artifact, not the
  bundle** — wac strips custom sections (finding #14).
- **`#[case]` names/tags must be literal/derivable at expansion**
  (compile-time section emission). Dynamic cases go through
  `#[case_generator]`, whose prefix record keeps the inventory honest;
  raw `Registry` registration bypasses inventory and will trip the
  runner's drift cross-check.
- The sample and fixture suites' expected outputs are asserted
  byte-for-byte by the `just verify-*` recipes against `expected/`
  (all four paths). If you change their cases or any runner/fold
  output format, regenerate the affected `expected/` files and the
  lockfiles, and commit the diffs.
