# js/runner-deltic

Host-side JS runner for `polymorph:test` suites on Deno via
[deltic](https://github.com/lann/deltic) — a runtime linker, so unlike
the jco leg there is no transpile step, no generated tree, and no engine
flag (the contract's async exports run on the callback ABI under stock
Deno). Same runner-is-provider topology as `js/runner-node`: deltic's
`runSuite` supplies `test-context` host-side and mirrors
`js/viewer/harness.mjs` case-loop semantics; `wasi()` serves the
suites' `wasi:{cli,clocks,io,random,filesystem}` leaves.

```sh
cargo build --target wasm32-wasip2 --release -p sample-suite   # from repo root
deno run --allow-read=target --config js/runner-deltic/deno.json --frozen \
    js/runner-deltic/runner.ts target/wasm32-wasip2/release/sample_suite.wasm \
    [--jsonl]
```

No fetch step and no extra permissions: the translator arrives with the
pinned `@deltic/translator` package and loads through the module graph
(`defaultTranslator()`). `--translator <wasm>` remains as a documented
escape hatch for an externally-sourced translator build.

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
caseTimeoutMs? }` and the worker loads `deltic-embedder.mjs`, built by
`just deltic-assets` from the pinned JSR graph (one platform-neutral
ES module: embedder API + Translator + runner glue + wasi shims). It drops into
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
which is what keeps `instanceof ComponentException` true across the host-module
boundary; workers resolve no import maps, so this is the only sound
shape. The stock `browser-worker.mjs` is `workerMain()` with the
bundleUrl-loading defaults.

## Pinning

deltic is consumed from JSR as **exact-pinned releases**:
`@deltic/{runtime,translator,wasi,ct-runner}` release in lockstep
from one upstream commit, and upstream is caret-honest (within a minor
line releases stay compatible; breaking changes bump the minor), so a
bump within a minor line is routine and a minor bump is a compatibility
review. The pin stays exact so the version is reviewable here and one
bump is one diff. Between releases every green deltic `main` commit
also publishes `<next>-pre.g<shorthash>` prereleases (unordered hash
versions — pin exactly) for when a not-yet-released commit is needed.

- `deno.json` — the import map holds the five `jsr:@deltic/...@<version>`
  specifiers. `@deltic/runtime/embedder` is mapped because
  `@deltic/wasi` imports it by bare specifier.
  `minimumDependencyAge` exempts the `@deltic` scope so same-day
  publishes resolve (Deno >= 2.9 for the wildcard exclude).
- `deno.lock` — carries JSR package integrity for that graph and is
  enforced with `--frozen` on every run, check, bundle, and
  `deno info` invocation.
- No sha256 bookkeeping and no release-asset downloads: the browser-leg
  assets are built from the same locked graph by `just deltic-assets`
  into `target/deltic-browser/` — `deltic-embedder.mjs` bundled from
  `browser-bundle-entry.ts`, and `deltic-translator-shim.wasm` copied
  out of the lock-pinned module cache (the packaged
  `@deltic/translator` asset). The directory is version-free: the lock
  owns versioning.
- `just deltic-pin-gate` (a `verify-deltic` prerequisite, so it runs in
  CI) asserts one version across every `@deltic` specifier in every
  `deno.json` and everything the lock resolves — the successor to the
  retired release-asset fetch script's `assertPinConsistency`.

To bump: update the version in `deno.json`'s import map, delete
`deno.lock`, re-run `deno install --entrypoint runner.ts
browser-bundle-entry.ts` in this directory, and commit the diff; the pin
gate asserts agreement. Regenerate the `expected/verify-deltic-*`
goldens only if an explained upstream behavior change moves them.
