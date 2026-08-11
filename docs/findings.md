# Toolchain findings log

Empirical findings from building and validating the stack, consolidated
from the original prototype and spike READMEs. Toolchain baseline:
wasmtime 47.0.1 (`-W component-model-async -S p3`), wac-cli 0.10.1,
wit-bindgen 0.60, jco 1.26.1, Node 24.18 (`--experimental-wasm-jspi`),
Rust `wasm32-wasip2` target.

## Contract / design findings

1. **`context.diagnostic` must be `async func` (L1).** wit-bindgen 0.60
   has no non-blocking stream write and no cross-task wake mechanism
   inside a component, so a sync `diagnostic` cannot feed a stream. With
   `async`, the provider awaits the write and backpressure replaces
   buffering: "never block the case" became "may block cooperatively;
   observing runners must drain concurrently".
2. **Composition factors as bundle-then-plug.** `wac plug` cannot build
   the provider topology in one step: plugging provider into suite
   "succeeds" but silently drops the provider's `factory`/`test-context`
   exports, and the resulting bundle fails runner linking with the
   opaque `type mismatch in instance export 'context'` — without the
   re-exported `test-context`, the `context` resource type cannot be
   proven identical across the runner's imports. A small `wac` script
   bundling suite + provider (re-exporting `tests`, `test-context`,
   `factory` from one shared provider instance) restores plug-ability.
3. **Fully-sync suites work unchanged** (verified by rebuilding the
   sample suite with `async: false`): blocking transfers control to the
   async runner, whose concurrent stream drain unblocks the provider and
   in turn the suite. `async` in the contract does not color suite
   toolchains. Corollary: the runner's drain obligation is load-bearing
   for sync suites too — a non-draining observer wedges them.
4. **Returned streams are async-lift-only on the producing side.** A
   sync-lifted implementation has no post-return execution window and a
   pre-return write deadlocks (streams are unbuffered rendezvous); a
   producer can only feed a returned stream across *later* calls into
   the instance. Checklist item for any L1 or provider-surface addition.

## wit-bindgen (guest)

5. **Dropping an in-flight stream read loses data** already copied into
   its buffer (write-completion and run-completion can arrive in one
   wake). Keep read state in the stream object
   (`StreamReader::into_stream()`, feature `futures-stream`), and drain
   diagnostics *biased before* taking the verdict.
6. **The wasip3 crate (0.7.0) is unusable alongside wit-bindgen 0.60
   bindings** (pins wit-bindgen 0.57; stream types don't unify across
   versions and `spawn` breaks — wit-bindgen #1305). Generate all
   bindings from one `generate!` world with vendored WIT instead.
7. Guest components built via `wasm32-wasip2` additionally import wasi
   0.2 interfaces (Rust std); wasmtime with 0.2 defaults plus `-S p3`
   satisfies the mix.

## wasmtime 47 (host embedding)

8. **Async-lifted exports are callable via plain `Func::call_async`**
   (routed through the concurrent machinery internally);
   `Config::wasm_component_model_async(true)`. No explicit
   `run_concurrent`/`Accessor` needed at call sites.
9. Host-side async imports: `LinkerInstance::func_wrap_concurrent` +
   `Accessor::with`; host resources passed as `borrow` params via
   `Resource::new_own` → `try_into_resource_any`.
10. Dynamic export names for resource methods: `[method]test-case.run`,
    receiver as the first `Val`. `result<_, outcome>` arrives as
    `Val::Result` with `Val::Variant("failed"|"skipped", ...)` payloads.
11. wasmtime 47 has its own error type (not anyhow): use
    `wasmtime::error::{Context, bail, format_err}` in embedding code.

## jco 1.26 / Node 24

12. **jco-node works via the host-provider topology**: transpile the
    suite alone (`--async-mode jspi`, `--map` `test-context` to a JS
    module), implement `Context` as a JS class, drive `tests` from JS.
    Async exports arrive as Promises; `result` err maps to a thrown
    exception with typed `payload`; JSPI suspends async host imports.
    Output matches wasmtime exactly. See `js/runner-node/`.
13. **Composed multi-component artifacts are blocked in jco today**:
    (a) handle tables are allocated per exported *interface*, not per
    resource type, so the bundle's `context` minted via
    `factory.new-context` is rejected by `tests.run` (`Not a valid
    "Context" resource`) — the same cross-interface aliasing
    wac+wasmtime unify; (b) async-lifted exports of composed artifacts
    fail with `missing global current task globalTaskMeta` (the
    `@bytecodealliance/preview3-shim` wasi:cli@0.3 boundary itself is
    fine). Both deserve upstream issues; reproducers in
    `js/runner-node/`.

## Custom sections (tags inventory)

14. Survival: wasm-component-ld (wasip2 build) ✅; jco transpile ✅
    (lands in `*.core.wasm`, statically readable); **`wac compose`
    strips custom sections** ❌ — generate lockfiles from the suite
    artifact pre-composition (where the lockfile's artifact hash binds
    anyway).

## Build reproducibility

15. **Suite wasm builds are not reproducible across checkouts**: rustc
    embeds absolute source paths (panic locations, debug info) in the
    artifact, so a source-identical build on another machine (or CI)
    yields a different sha256. `--remap-path-prefix`/`trim-paths` can
    mitigate but push the burden onto every suite's build config —
    ruled instead (#44) that artifact hashes are provenance, never a
    cross-environment gate: `lock --check` compares inventory,
    `aggregate` requires no lockfile↔envelope hash equality (only
    warning on cross-target disagreement within one run, which is
    reproducibility-independent).

## Case budgets (epochs + timers, #45)

16. **`epoch_deadline_callback` composes with component-model-async**
    (wasmtime 47): the callback fires during guest execution of
    async-lifted exports; an `Err` returned from it unwinds the
    in-flight `call_async` as that error, and the concrete error type
    survives for `downcast_ref` at the call site (the runner also
    keeps a Display-substring fallback). Missed ticks collapse into
    one callback on resume, so tick-counting in the callback
    approximates *execution* time and under-counts OS preemption —
    the right direction for a budget.
17. **`wasmtime_wasi::runtime::in_tokio` has timers enabled** —
    `tokio::time::sleep` works on that runtime (it already powers
    wasi clock pollables), so a wall-clock race against the run
    future needs no extra runtime plumbing.
18. **`std::future::pending().await` does not wedge a wit-bindgen 0.60
    guest — it traps**: the guest runtime refuses to park a task
    waiting only on Rust-originating events (panic: "cannot sleep
    waiting only on Rust-originating events unless … the
    `inter-task-wakeup` feature") unless that feature is enabled. A
    genuine async wedge requires a *host*-originating wait (e.g. a
    WASI sleep: `std::thread::sleep` on wasip2 suspends in the host's
    async `poll`, no wasm executing). Hence `hang/wedge` sleeps via
    WASI rather than awaiting `pending()`.

## Handle mint/lift at scale (bench-suite; #22/#25 context)

Synthetic measurement of the `all()` protocol per fresh instance —
N trivially-passing cases, no corpus, no per-case data
(`components/bench-suite`, count via `BENCH_CASES` env). Drivers:
`bench-mint` bin (wasmtime, production `Runner` config: pooling, CoW,
epoch instrumentation, untyped `Val` calls) and
`js/runner-deltic/bench-mint.mjs` (pinned deltic embedder, plain
Node). Medians over 20/10 fresh instances, one dev box (17-core
x86_64 Linux), wasmtime 47.0.3 / deltic pre-83fff30 / Node 24.

19. **wasmtime: `all()` splits ~3:1 registry-build : mint+lift, both
    linear.** At 10k cases: all#1 (build + mint + lift) 3.2ms, all#2
    (mint + lift only, registry cached) 0.79ms ≈ **75–80ns/handle**;
    registry build (the OnceCell `IndexMap` + boxed-closure loop) ≈
    240ns/case. Instantiate 21µs; store drop 83µs at 10k (scales with
    lifted-handle count); `name`/`run` boundary calls 2–3µs each.
    Instance-per-case on a 10k suite therefore pays ~3.3ms/case of
    pure protocol overhead (matches #22's campaign arithmetic), and
    the guest-side registry build — not the handle lift — is the
    larger share, so SDK-side table work (static case table, #25
    wizer) buys more than lift avoidance alone; a direct-access
    interface caps out at ~25% unless stacked on a lazy/static
    registry.
20. **deltic (runtime linker, callback ABI): same shape, bigger
    constants.** Instantiate ~650µs (~30× wasmtime); mint+lift
    ~340–370ns/handle (~4.3×; mildly superlinear by 30k — V8 GC on
    the wrapper objects); per-call boundary overhead ~25µs (~10×);
    all#1 at 10k ≈ 5.2ms. Translator init + component translate are
    one-off ~16ms + ~25ms. Per-fresh-instance topologies are
    tolerable here only while per-instance work stays O(cases
    served), not O(suite).
21. **`harness.mjs` fresh-instance relocation is the real JS-leg
    quadratic**: `freshCases` re-finds the case by a linear
    `name()` scan (harness.mjs `String(await c.name()) === name`), ≈
    N/2 × 25µs ≈ **130ms per case** at 10k — ~20× the `all()` cost it
    sits on top of. The contract guarantees `all()` order is
    deterministic across instances, so positional relocation (index
    into the fresh list + one `name()` verify) is sound and erases
    the scan.

