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
    js/runner-deltic/fetch-translator.ts)
deno run --allow-read=target --config js/runner-deltic/deno.json --frozen \
    js/runner-deltic/runner.ts target/wasm32-wasip2/release/sample_suite.wasm \
    --translator "$translator" [--jsonl]
```

Human mode reproduces `expected/verify-run-sample.txt` byte-for-byte
(shared with the composed-cli and jco legs); `--jsonl` emits canonical
L4 results JSONL (`just verify-deltic` diffs it, normalized, against
`expected/verify-deltic-*.jsonl` and folds it through `component-test
fold` against the shared sample fold golden).

## Pinning

deltic is pinned to a release tag in **two** places, cross-checked at
run time by `fetch-translator.ts`:

- `deno.json` — import-map URLs (`raw.githubusercontent.com/lann/deltic/<tag>/…`);
  `deno.lock` carries integrity hashes for that module graph and is
  enforced with `--frozen`. The `@deltic/runtime/embedder` entry exists
  because `wasi-shims` imports it by bare specifier (resolved by the
  workspace config inside deltic; URL consumers must map it).
- `fetch-translator.ts` — `TAG` + `TRANSLATOR_SHA256` for the
  `deltic-translator-shim.wasm` release asset (cached under
  `target/deltic/<tag>/`).

To bump: update the tag in both files and the sha from the release's
`SHA256SUMS`, delete `deno.lock`, re-run
`deno cache runner.ts fetch-translator.ts` in this directory, commit the
diff. Scheduling is feature-blind (`scheduling: "none"`, like the
composed runner): the fixture leg *executes* tag-gated cases instead of
scheduling them out, so it has its own goldens.
