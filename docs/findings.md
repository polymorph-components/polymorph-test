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

## Custom sections (marks inventory)

14. Survival: wasm-component-ld (wasip2 build) ✅; jco transpile ✅
    (lands in `*.core.wasm`, statically readable); **`wac compose`
    strips custom sections** ❌ — generate lockfiles from the suite
    artifact pre-composition (where the lockfile's artifact hash binds
    anyway).
