# component-test viewer

A static page (`index.html`) with two panes over the stack's canonical
formats — the human face of a suite's results, and a way to run one
live in the browser you're sitting at.

**Results**: load a lockfile, a target manifest, and per-target
results-JSONL streams; the page renders the target × case matrix,
per-case detail (status, provenance, detail line, the diagnostics
sideband), and the validation surface (errors and warnings, always
shown — the viewer must not look greener than the gate). Aggregation is
not a JS re-implementation: it runs `viewer-aggregate`
(`components/viewer-aggregate`), the gate's own Rust join/validation
code compiled to a component, so the page's verdicts are
`component-test aggregate`'s verdicts by construction.

**Live run**: point at a jco transpile of any suite, choose the
missing-features declaration and a worker count, and the suite runs in
a Web Worker pool — striped `index % workers` like every other runner,
one suite instance per worker — streaming rows into the page. A
finished run downloads as results-JSONL or feeds straight into the
Results pane as one target's stream. Live runs need JSPI (Chrome 137+).

```sh
just viewer-serve    # build engines + serve the repo root; open the printed URL
just verify-viewer   # the drift gate: both engines under Node (CI runs this)
```

## The demo arc

"Load demo" loads the fixture-suite walkthrough: the committed
`--missing hsm` stream as the manifest's `sim` target, leaving `native`
reporting nothing — an error, on purpose. The Live pane's defaults
produce exactly the missing leg (fixture suite, hsm present, labeled
`native`); "Send to Results" completes the matrix: OK, with the
deliberate trap showing as tracked expected-fail debt on both targets.

## Layout

- `harness.mjs` — the browser-safe runner core: tag-inventory parsing
  (custom sections), mark scheduling, the per-case loop with shard
  striping. Shared by the page's workers and the Node selftest; the
  future gating jco adapter (#5) must use this same module — the gate
  and the page must not drift.
- `context.js` — the host-implemented `test-context` provider.
- `worker.mjs` — one shard of a live run (module workers cannot see
  import maps, which is why every transpile here maps imports to
  relative paths).
- `app.mjs`, `index.html`, `viewer.css` — the page.
- `selftest.mjs` — the drift gate `just verify-viewer` runs: the wasm
  aggregate must reproduce the CLI's verdicts over the fixture
  pipeline; the harness must run the transpiled suites to the
  documented verdicts, including striping partition equality.
- `generated/`, `suite/` — transpiles (gitignored; `just viewer-build`).

The page is thin glue over the two verified engines; browser-only
plumbing (worker messaging, rendering) has no gate beyond the demo arc
above — keep logic in the engines.
