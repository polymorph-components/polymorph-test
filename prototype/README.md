# Prototype: end-to-end validation

A working wasip3/component-model-async prototype of the full contract:

- `provider/` — reference context provider (`test-context` + `factory`):
  linked (context, observer) pairs; diagnostics delivered over
  `stream<string>`; black hole until observed; stream closes on context
  drop.
- `suite/` — sample suite: `sample/math/add` (passes), `sample/math/mul`
  (fails), and `sample/token/attest` (runtime `skipped` — the exceptional
  escape hatch for run-stable target facts that fail to hold at run time),
  all emitting diagnostics.
- `runner/` — wasip3 CLI runner core: exports `wasi:cli/run@0.3.0`,
  executes cases sequentially, drains each case's diagnostics stream
  concurrently with its `run`, write-through to stdout.
- `bundle.wac` — step 1: bundle suite + provider into a component that
  exports `tests`, `test-context`, and `factory` from one shared provider
  instance. Step 2 is then a plain `wac plug` of the bundle into any
  runner core. (`compose.wac` is the equivalent one-shot three-node
  composition.) `wac plug` alone can't perform step 1 — it would drop the
  provider's `factory`/`test-context` exports — but once bundled, the
  runner-facing step is the standard plug operation. (Verified: plugging
  provider into suite directly "succeeds" but drops those exports, and the
  resulting bundle fails step 2 with `type mismatch in instance export
  'context'` — without the re-exported `test-context`, the `context`
  resource type can't be proven identical across the runner's imports.)

## Build & run

```sh
cargo build --target wasm32-wasip2 --release
# Step 1: bundle suite with its context provider
wac compose \
  -d lann:provider-impl=target/wasm32-wasip2/release/provider.wasm \
  -d lann:sample-suite=target/wasm32-wasip2/release/suite.wasm \
  -o bundle.wasm bundle.wac
# Step 2: plug the bundle into a runner core
wac plug --plug bundle.wasm target/wasm32-wasip2/release/runner.wasm -o composed.wasm
wasmtime run -W component-model-async -S p3 composed.wasm
```

Expected output (exit code 1: a case failed):

```
test sample/math/add ...
    diag: computing 2 + 2
    diag: got 4
test sample/math/add: PASS
test sample/math/mul ...
    diag: computing 6 * 9
    diag: got 54, expecting the ultimate answer
test sample/math/mul: FAIL: 6 * 9: expected 42, got 54
test sample/token/attest ...
    diag: probing for hardware token
    diag: token unavailable; asserting clean error
test sample/token/attest: SKIP: token unavailable at run time; asserted attestation fails cleanly (no hang, no partial attestation)

result: 1 passed, 1 failed, 1 skipped, 3 total
```

Note: `all()` is parameterless and `test-case` carries no `features()` —
feature marks are static metadata outside the WIT contract (see the main
README), and the scheduler-side `not-applicable` status is a results-format
concept, not demonstrated here.

Toolchain validated against: wasmtime 47.0.1 (`-W component-model-async
-S p3`), wac-cli 0.10.1, wit-bindgen 0.60, Rust `wasm32-wasip2` target.

## Findings that fed back into the design

1. **`context.diagnostic` must be `async func` (L1 change).** wit-bindgen
   0.60 has no non-blocking stream write and no cross-task wake mechanism
   inside a component, so a sync `diagnostic` cannot feed a stream (the
   planned "provider buffers + pumps" design is unimplementable). With
   `async`, the provider simply awaits the write and backpressure replaces
   buffering. "Never block the case" became "may block cooperatively;
   observing runners must drain concurrently".
2. **Composition factors as bundle-then-plug.** `wac plug` can't build the
   provider topology in one step, but a small `wac` script bundling
   suite + provider (re-exporting `tests`, `test-context`, `factory` from
   one shared provider instance) restores plug-ability: the bundle
   satisfies every runner-core import, so the runner-facing step is a
   plain `wac plug`. The `context` resource type stays unified across the
   bundle's exports.
3. **Dropping an in-flight stream read loses data.** A `select!` loop that
   recreates `StreamReader::next()` futures per iteration can drop a read
   whose buffer was already filled (write-completion and run-completion
   arriving in the same wake). Runner cores must keep read state in the
   stream object (`into_stream()`, wit-bindgen's `futures-stream` feature)
   — and drain diagnostics *biased before* taking the verdict.
4. **The wasip3 crate (0.7.0) is unusable alongside wit-bindgen 0.60
   bindings** (pins wit-bindgen 0.57; stream types don't unify across
   versions and `spawn` breaks — wit-bindgen #1305). Generating all
   bindings from one `generate!` world, with wasi:cli@0.3.0 WIT vendored
   into `runner/wit/deps`, avoids this.
5. Guest components built via the `wasm32-wasip2` target additionally
   import wasi 0.2 interfaces (Rust std); wasmtime with both 0.2 defaults
   and `-S p3` satisfies the mix.
6. **Fully-sync suites work unchanged** (verified by rebuilding the suite
   with `async: false`: sync-lifted `run`, sync-lowered `diagnostic`).
   Blocking transfers control back to the async runner, whose concurrent
   stream drain unblocks the provider and in turn the suite — identical
   output, live diagnostics included. `async` in the contract does not
   color suite toolchains. Corollary: the runner's drain obligation is
   load-bearing for sync suites too — a non-draining observer wedges
   them just the same.
