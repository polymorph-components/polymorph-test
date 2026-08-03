# AGENTS.md

Orientation for coding agents (and other newcomers) working in this
repository. Read this before making changes; most expensive mistakes
here are edits to frozen surfaces or verification steps skipped.

## What this is

`lann:component-test`: common infrastructure for testing WebAssembly
components. A small frozen WIT contract between test *suites* and test
*runners*, plus the tooling around it: guest SDK, inventory lockfiles,
capability manifests via feature marks, a canonical results format,
runners (wasmtime host-embed, composed wasi:cli, jco/Node), and a CLI.

Design intent lives in three documents, in decreasing authority:

- `README.md` — the contract, the feature-mark scheme, and the design
  commitments (each is load-bearing; do not weaken one without explicit
  direction).
- `ARCHITECTURE.md` — the layer model (L0 producers … L6 workflow), the
  stability gradient, and the one sanctioned layer violation.
- `docs/findings.md` — empirical toolchain findings. When you discover
  new toolchain behavior (wasmtime/wit-bindgen/jco/wac), append it here.

Work is tracked in GitHub issues (`gh issue list -R lann/component-test`).
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
  component-test-core      name grammar, marks, verdicts
  component-test-formats   lockfile, results model/JSONL, inventory scanner
  component-test-sdk       guest SDK (registry, case!, marks section emission)
  component-test-cli       `component-test` bin: lock, fold
  component-test-runner    wasmtime host-embed runner (`ct-runner` bin)
components/           guest components (build with --target wasm32-wasip2)
  provider                 reference context provider
  runner-cli               composed wasi:cli runner core
  sample-suite             demo suite (pass/fail/runtime-skip)
  fixture-suite            runner-testing fixture (trap case, marked pair)
js/runner-node/       Node runner via jco (runner-is-provider topology)
examples/compose/     wac composition walkthrough (bundle-then-plug)
docs/findings.md      toolchain findings log
```

**Gotcha: WIT copies.** `components/*/wit/` contain vendored copies of
`wit/tests.wit` (and `components/provider/wit/provider.wit` is the
canonical provider WIT, vendored into `components/runner-cli/wit/deps/`).
After editing any WIT, re-sync the copies and re-validate:

```sh
for d in components/provider/wit/deps/component-test \
         components/runner-cli/wit/deps/component-test; do cp wit/tests.wit $d/; done
cp wit/tests.wit components/sample-suite/wit/tests.wit
cp wit/tests.wit components/fixture-suite/wit/tests.wit
wasm-tools component wit wit/
```

## Toolchain

wasmtime 47 (`-W component-model-async -S p3`), wac-cli 0.10, wit-bindgen
0.60, jco 1.26 (via npx), Node 24 (`--experimental-wasm-jspi`), Rust
target `wasm32-wasip2`. Known sharp edges are catalogued in
`docs/findings.md` — read it before fighting the toolchain; your bug is
probably finding #5, #6, or #13.

## Build & verify

Host crates (fast, run this always):

```sh
cargo test --workspace --exclude sample-suite --exclude provider \
           --exclude runner-cli --exclude fixture-suite
```

Guest components:

```sh
cargo build --target wasm32-wasip2 --release \
  -p sample-suite -p provider -p runner-cli -p fixture-suite
```

Full verification matrix (run whatever your change touches; all four
before committing anything cross-cutting):

1. **Host-embed runner** (also exercises marks scheduling + trap path):
   ```sh
   cargo run -q -p component-test-runner --bin ct-runner -- \
     target/wasm32-wasip2/release/sample_suite.wasm
   cargo run -q -p component-test-runner --bin ct-runner -- \
     target/wasm32-wasip2/release/fixture_suite.wasm --missing hsm
   ```
   Expected: sample = 1 passed / 1 failed / 1 skipped; fixture = 3
   passed / 1 failed (trap) / 1 N/A. Exit code 1 in both.
2. **Composed runner** (see `examples/compose/README.md`): bundle via
   `wac compose`, plug via `wac plug`, run under `wasmtime run -W
   component-model-async -S p3`. Same sample-suite verdicts.
3. **jco-node runner** (see `js/runner-node/README.md`): transpile the
   suite alone with `--async-mode jspi` and drive from Node. Same
   verdicts.
4. **Inventory + results pipeline**:
   ```sh
   cargo run -q -p component-test-cli -- lock \
     target/wasm32-wasip2/release/sample_suite.wasm \
     --check components/sample-suite/tests.lock
   cargo run -q -p component-test-runner --bin ct-runner -- \
     target/wasm32-wasip2/release/sample_suite.wasm --jsonl \
     | cargo run -q -p component-test-cli -- fold components/sample-suite/tests.lock
   ```

Lockfiles (`components/*/tests.lock`) are generated artifacts bound to
the suite wasm by sha256: regenerate with `component-test lock ... -o`
after any suite change and commit the diff (the diff *is* the review
surface). Never hand-edit.

## Conventions

- Rust 2021, workspace deps in the root `Cargo.toml`. `crates/` =
  host-side (must build and test natively); `components/` = guest-side
  (wasm32-wasip2 only; native `cargo test` will fail on them — that's
  why the test command above excludes them).
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
- **Marks/lockfile generation reads the suite artifact, not the
  bundle** — wac strips custom sections (finding #14).
- **The `case!` macro requires literal names/marks** (compile-time
  section emission); dynamic registration bypasses inventory and will
  trip the runner's drift cross-check.
- The sample suite's expected output is asserted byte-for-byte in
  multiple places (runner acceptance, examples, jco README). If you
  change its cases, update all of them and the lockfile.
