# Composition examples

Two-step (canonical): bundle the suite with the reference context
provider, then plug the bundle into a runner core.

```sh
cargo build --target wasm32-wasip2 --release -p sample-suite -p provider -p runner-cli
wac compose \
  -d lann:provider-impl=../../target/wasm32-wasip2/release/provider.wasm \
  -d lann:sample-suite=../../target/wasm32-wasip2/release/sample_suite.wasm \
  -o bundle.wasm bundle.wac
wac plug --plug bundle.wasm ../../target/wasm32-wasip2/release/runner_cli.wasm -o composed.wasm
wasmtime run -W component-model-async -S p3 composed.wasm
```

`COMPONENT_TEST_JSONL=1` (env) switches the composed runner to the
results wire format; fold it with `component-test fold`.

`compose.wac` is the equivalent one-shot three-node composition.

Note: `wac plug` alone cannot build the bundle — it drops the
provider's `factory`/`test-context` re-exports, and the result fails
runner linking with `type mismatch in instance export 'context'` (see
docs/findings.md). Custom sections (the marks inventory) are stripped
by `wac compose`: generate lockfiles from the suite artifact, not the
bundle.
