# M1.0 jco-node spike (#27)

Verdict: **jco-node is viable for M1 via the host-provider topology**;
composed multi-component artifacts are not viable under jco today.

Toolchain: jco 1.26.1, Node 24.18.0 (`--experimental-wasm-jspi`),
`@bytecodealliance/preview2-shim`, suite built for wasm32-wasip2.

## What works (exit criteria met)

Transpile the **suite alone** with `--async-mode jspi`, map
`test-context` to a host JS module, drive `tests` from a JS runner:

```sh
jco transpile ../target/wasm32-wasip2/release/suite.wasm \
  --async-mode jspi \
  --map 'lann:component-test/test-context@0.1.0=../context.js' \
  -o suite
node --experimental-wasm-jspi runner-host-provider.mjs
```

Output matches `wasmtime -W component-model-async -S p3` exactly
(PASS/FAIL/SKIP + live diagnostics, `1 passed, 1 failed, 1 skipped`).
Working surface: async-lifted exports (`all`, `run`) as Promises; JS-side
resource implementation (`Context` class) passed as `borrow` into `run`;
async host import (`diagnostic`) suspending via JSPI; `result<_, outcome>`
err mapped to thrown exception with typed `payload`.

The JS runner **is** the provider (webcrypto host-implemented-imports
pattern): no wasm provider component, no diagnostics stream needed —
`diagnostic` lands host-side directly. Architecturally this is the jco
analog of the wasmtime host-embed runner.

## Blockers found (composed artifacts)

1. **Cross-interface resource aliasing breaks.** Transpiling the composed
   bundle (suite + provider): jco allocates handle tables **per exported
   interface**, not per resource type, so the `context` minted by
   `factory.new-context` (table 7) is rejected by `tests.run`'s validation
   (table 19) — `TypeError: Not a valid "Context" resource`. wac+wasmtime
   unify the same aliasing correctly. Minimal reproducer: transpile
   `prototype/bundle.wasm`, call `newContext()` then `run(ctx)`.
2. **Async task bookkeeping fails for internal cross-component calls.**
   Driving the composed wasi:cli artifact's async-lifted `run` export
   (with `@bytecodealliance/preview3-shim` for wasi:cli@0.3 stdout) fails
   with `missing global current task globalTaskMeta`. Note: a
   `preview3-shim` exists and jco knows to import it — the 0.3 shim
   boundary itself is not the blocker.

Both deserve upstream jco issues with reproducers from this directory.

## M1 consequences

- jco-node targets use: transpiled suite + JS host provider + JS runner.
- The provider component and composed runner remain wasmtime-scoped.
- The L4 typed-results interface will meet the same host boundary: the
  JS runner serializes (it is host-side), consistent with #26.
