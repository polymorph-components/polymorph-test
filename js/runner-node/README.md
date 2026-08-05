# js/runner-node

Host-side JS runner for `polymorph:component-test` suites on Node via jco —
the jco analog of the wasmtime host-embed runner: the runner *is* the
context provider (`context.js` implements `test-context`), and the
suite is transpiled alone (composed multi-component artifacts are
blocked in jco today — see `docs/findings.md` #12–13).

```sh
cargo build --target wasm32-wasip2 --release -p sample-suite   # from repo root
npm install
npx @bytecodealliance/jco transpile ../../target/wasm32-wasip2/release/sample_suite.wasm \
  --name suite \
  --async-mode jspi \
  --map 'polymorph:component-test/test-context@0.1.0=../context.js' \
  -o suite
node --experimental-wasm-jspi runner-host-provider.mjs
```

`runner.mjs` is the retained reproducer for the composed-bundle handle
table blocker (transpile `examples/compose/bundle.wasm`, call
`newContext()` then `run(ctx)`).
