# examples/aggregate

The L4→L5 pipeline end to end: run one suite against two declared
targets, then join the result streams against the lockfile and the
target manifest (`targets.toml`) into a validated cross-target matrix.

```sh
# from the repo root, after `just build`
component-test lock target/wasm32-wasip2/release/fixture_suite.wasm -o /tmp/tests.lock

ct-runner target/wasm32-wasip2/release/fixture_suite.wasm \
  --jsonl --target native                > /tmp/native.jsonl   # exit 1: the trap case
ct-runner target/wasm32-wasip2/release/fixture_suite.wasm \
  --jsonl --target sim --missing hsm     > /tmp/sim.jsonl      # exit 1: same

component-test aggregate \
  --lock /tmp/tests.lock --manifest examples/aggregate/targets.toml \
  --results native=/tmp/native.jsonl --results sim=/tmp/sim.jsonl \
  -o matrix.md                                                 # exit 1: failures present
```

What the aggregator checks before rendering anything: closed feature
namespace in both directions, dead coverage (every case applicable on
at least one target), per-target coverage against the lockfile,
artifact-sha256 binding (envelope vs lockfile — which is why the
walkthrough regenerates the lockfile from the artifact it runs),
applicability drift, unterminated segments, run errors.

`just verify-aggregate` runs exactly this and diffs `matrix.md`
against `expected/verify-aggregate-matrix.md`.

Note the composed (wasi:cli) runner is execute-everything — wac strips
the tags section (findings #14), so it cannot schedule. Its streams
declare `"scheduling":"none"` in the envelope, and the aggregator
*applies* applicability for them (executed non-applicable cases are
reclassified to `not-applicable`, with a warning) instead of policing
it. Host-embed streams declare `"scheduling":"tags"` and stay under
the strict drift gate.
