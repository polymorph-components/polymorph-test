# js/runner-deltic

Host-side JS runner for `polymorph:test` suites on Deno via
[deltic](https://github.com/lann/deltic) — a runtime linker, so unlike
the jco leg there is no transpile step, no generated tree, and no engine
flag (the contract's async exports run on the callback ABI under stock
Deno). Same runner-is-provider topology as `js/runner-node`: deltic's
`runSuite` supplies `test-context` host-side and mirrors
`js/viewer/harness.mjs` case-loop semantics; `wasiShims()` serves the
suites' `wasi:{cli,clocks,io,random,filesystem}` leaves.

```sh
cargo build --target wasm32-wasip2 --release -p sample-suite   # from repo root
translator=$(deno run --allow-read=js/runner-deltic,target --allow-write=target \
    --allow-net=github.com,objects.githubusercontent.com,release-assets.githubusercontent.com \
    js/runner-deltic/fetch-deltic.ts --asset translator)
deno run --allow-read=target --config js/runner-deltic/deno.json --frozen \
    js/runner-deltic/runner.ts target/wasm32-wasip2/release/sample_suite.wasm \
    --translator "$translator" [--jsonl]
```

Human mode reproduces `expected/verify-run-sample.txt` byte-for-byte
(shared with the composed-cli and jco legs); `--jsonl` emits canonical
L4 results JSONL (`just verify-deltic` diffs it, normalized, against
`expected/verify-deltic-*.jsonl` and folds it through `component-test
fold` against the shared sample fold golden). Tag scheduling comes from
the suite's own embedded inventory (deltic#25): the fixture leg runs
`--missing hsm` exactly like the embed leg and schedules `hsm`-marked
cases out as `not-applicable`.

## Browser leg

`browser-worker.mjs` is the runtime-linked sibling of
`js/viewer/browser-worker.mjs` — same reply protocol, same shared
`harness.mjs` case loop (striping, freshCases, timeouts, mark
scheduling), no transpiled artifacts: the run message carries
`{ bundleUrl, translatorUrl, suiteUrl, env?, missing?, only?, shard?,
caseTimeoutMs? }` and the worker loads the pinned `deltic-embedder.mjs`
release asset (one platform-neutral ES module: embedder API +
Translator + runner glue + wasi shims). It drops into
`page-runner.mjs`'s `runSuitesInPage` via its `workerUrl` parameter —
page runner and browser driver unchanged. `engine.mjs` is the shared
glue; `selftest.mjs` drives the same engine path under plain `node`
(NO `--experimental-wasm-jspi` — the callback ABI needs no engine
flag), asserting the documented sample/fixture verdicts, trap
containment, tag scheduling, and striping partition equality; it runs
as `verify-deltic`'s last leg.

Suites that import a SUT host module (`polymorph:websocket`,
`polymorph:webcrypto`, …) use `worker-main.mjs` (exported as
`./deltic-worker-main`) instead of the stock worker: the downstream
repo bundles one worker entry — the deltic engine surface, the message
loop, and its own host module, resolved through one import map — and
passes `workerMain({ deltic, suiteImports })` its inlined engine and an
import-record factory. One bundle means one embedder module instance,
which is what keeps `instanceof WitError` true across the host-module
boundary; workers resolve no import maps, so this is the only sound
shape. The stock `browser-worker.mjs` is `workerMain()` with the
bundleUrl-loading defaults.

## Pinning

deltic is pinned to a release tag in **two** places, cross-checked at
run time by `fetch-deltic.ts`:

- `deno.json` — import-map URLs (`raw.githubusercontent.com/lann/deltic/<tag>/…`);
  `deno.lock` carries integrity hashes for that module graph and is
  enforced with `--frozen`. The `@deltic/runtime/embedder` entry exists
  because `wasi-shims` imports it by bare specifier (resolved by the
  workspace config inside deltic; URL consumers must map it).
- `fetch-deltic.ts` — `TAG` + per-asset sha256 for the
  `deltic-translator-shim.wasm` and `deltic-embedder.mjs` release assets
  (cached under `target/deltic/<tag>/`; `--asset translator|embedder`).

To bump: update the tag in both files and the shas from the release's
`SHA256SUMS`, delete `deno.lock`, re-run
`deno cache runner.ts fetch-deltic.ts` in this directory, regenerate the
`expected/verify-deltic-*` goldens, and commit the diff.
