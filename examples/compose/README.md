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

`--env COMPONENT_TEST_JSONL=1` (wasmtime does not inherit host env
vars) switches the composed runner to the results wire format; fold it
with `component-test fold`:

```sh
wasmtime run -W component-model-async -S p3 --env COMPONENT_TEST_JSONL=1 composed.wasm \
  | component-test fold ../../components/sample-suite/tests.lock
```

This runner is execute-everything: wac strips the tags custom section
(findings #14), so it cannot apply feature-tag scheduling. Its
envelope says so (`"scheduling":"none"`), and `component-test
aggregate` applies applicability for such streams from the lockfile +
target manifest — executed non-applicable cases are reclassified to
`not-applicable`, matching the host-embed runner's output shape (see
`examples/aggregate/`).

`compose.wac` is the equivalent one-shot three-node composition.

Note: `wac plug` alone cannot build the bundle — it drops the
provider's `factory`/`test-context` re-exports, and the result fails
runner linking with `type mismatch in instance export 'context'` (see
docs/findings.md). Custom sections (the tags inventory) are stripped
by `wac compose`: generate lockfiles from the suite artifact, not the
bundle.
