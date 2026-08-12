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
21. **`harness.mjs` fresh-instance relocation was the real JS-leg
    quadratic** (fixed — positional relocation, PR #83): `freshCases`
    re-found each case by a linear `name()` scan. Hot-loop `name()`
    costs ~3.4µs under deltic (a cold single call measures ~26µs —
    promise/JIT overhead that amortizes), so the scan averaged ~N/2 ×
    3.4µs ≈ **17ms per case** at 10k (33ms worst, measured), a
    multiple of the `all()` re-enumeration it followed and O(N²)
    across a run. The contract guarantees `all()` order is
    deterministic across instances, so positional relocation (index +
    one `name()` verify) is sound and erases it: 33.4ms → 0.0ms for
    the last case at 10k.

## Component-level wizer pre-initialization (#25)

Follow-up to the bench above: pre-build the registry at build time so
fresh instances are born initialized. `wasmtime-wizer` 47 as a
library; drivers: originally the `wizer-preinit` bin, since #85 the
`wizen` module in component-test-runner (feature `wizer`) behind
`component-test wizen`; measured over the
bench-suite artifact built with its `wizer-init` feature.

22. **Component-level pre-init works today (wasmtime-wizer 47) — #25's
    "core-module level only" constraint is stale — and the suite needs
    no init export: the contract's own `all()` is the init function.**
    Named in the *version-last* invoke syntax,
    `polymorph:test/tests.all@0.1.0()` — the wave/`ItemName` grammar
    places `@version` after the item name to resolve the dot
    ambiguity (`pkg:ns/iface.func@1.2.3`), and rejects the
    export-name-order form `pkg:ns/iface@1.2.3.func` by design (the
    resulting "invalid token" error points at the run-command docs
    and hints at neither). The parenthesized wave-call form is
    required: the bare item-name path demands a `[] -> []` signature,
    and `all` has a result. The returned handles are per-call state;
    only the built registry lands in the snapshot (measured: same
    size as via a dedicated no-op init export, ±3KB). Wizening a
    *suite* still needs wasmtime-wizer as a library —
    `Wizer::run_component` takes a caller-supplied instantiate
    closure, so a custom linker satisfies `test-context` (a host
    resource whose methods init never calls) plus full WASI (env
    reads during init work) — because the CLI cannot: unknown-import
    stubbing cannot synthesize **resource** types ("resource
    implementation is missing"), composed bundles fail ("nested
    components with modules not currently supported"), and the CLI
    additionally defaults WASI *off* for wizening (`-S cli`
    required for env-reading inits). Two upstream edges:
    `keep_init_func(false)` — moot here (stripping would remove
    `tests.all`) — emits an invalid component for dedicated init
    exports (dangling core-instance export reference; known,
    bytecodealliance/wasmtime#13168); and the wave func-name lexer's
    semver subpattern is bare `X.Y.Z`, so prerelease-versioned
    interfaces (`@0.3.0-rc-…`) cannot be named in call form. Custom
    sections **survive** the rewrite (tags inventory intact — the
    runner's scheduling and drift checks work on the wizened
    artifact, unlike wac-composed bundles, finding 14).
23. **wasmtime, wizened 10k suite: the registry-build half vanishes
    exactly.** all#1 3.15ms → 663µs ≈ all#2; instantiate unchanged
    (~19µs — CoW absorbs the 1.29MB snapshot, 122KB → 1.29MB); store
    drop 80µs → 12µs (fewer runtime-dirtied pages). End-to-end K=1
    full-isolation run: 30.8s → 7.1s sequential (4.3×), **1.14s at
    jobs=8** — per-case isolation on a wizened suite undercuts the
    shared-instance numbers that motivated relaxing isolation in #22.
24. **deltic, wizened suite: net ~1.5× only.** all#1 6.9ms → 2.9ms,
    but instantiate 0.78ms → 2.17ms: V8 has no CoW memory images, so
    the 1.29MB active data segment is copied at every instantiation,
    eating most of the build win (translate also 24ms → 44ms,
    one-off). Wizening pays on JS legs only when enumeration is
    genuinely expensive; instance-granularity K>1 remains the lever
    there.
25. **wasm-opt post-pass on wizened artifacts: no meaningful
    clawback, and blocked at the component level anyway.** binaryen
    v124 refuses components outright (binaryen#6728). The bound holds
    regardless: the wizened bench artifact's growth is snapshot
    *data* — 1.18MB across 10,002 data segments vs 93KB of code in
    the 1.29MB artifact (`wasm-tools objdump`) — so even total code
    deletion reclaims ≤7.2%. Measured on the extracted core module
    (byte range from objdump; no supported round-trip back into a
    component exists): `wasm-opt -Oz` saves 19.5KB, 1.5% of the
    artifact. #25's "init-only code goes dead after snapshotting"
    hypothesis is immaterial in this shape: the case bodies stay live
    through the snapshotted case table — only the one-shot
    registration driver dies, and it is small. CoW already makes the
    on-disk growth near-free at runtime (finding 23); transport
    compression covers the wire. `wasm-tools strip` is no substitute
    and a trap besides: it *does* process components, but saves only
    4.9KB (0.38% — `[profile.release] strip = true` already removed
    the heavy custom sections at build time), and its default keep
    list is `name`/`component-type`/`dylink.0` only, so it deletes
    `component-test:tags@0.1` — scheduling and `lock --check` break
    exactly as the CLI's "sections stripped" error anticipates. If it
    must run, name sections explicitly with `--delete`; never the
    default form.

