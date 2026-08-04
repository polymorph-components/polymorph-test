# component-test task runner. `just` lists recipes; `just all` is the
# full verification matrix from AGENTS.md.

wasm_target := "wasm32-wasip2"
release_dir := "target" / wasm_target / "release"
wasmtime_flags := "-W component-model-async -S p3"

_default:
    @just --list --unsorted

# Everything: host tests, component builds, all four verification paths.
all: build test test-wasm lock-check verify-embed verify-compose verify-node verify-pipeline verify-aggregate

# CI's native job: formatting, clippy, host tests, WIT validation.
host-checks: fmt-check lint test wit-check

fmt-check:
    cargo fmt --all --check

# Host crates only (workspace default-members excludes components/).
lint:
    cargo clippy --all-targets -- -D warnings

# Host-crate tests (fast; default-members excludes the wasm-only
# component crates).
test:
    cargo test

# Integration tests that execute built components (`#[ignore]`d in
# plain `cargo test`; artifacts come from `just build`).
test-wasm: build
    cargo test -p component-test-runner -p component-test-cli \
        --tests -- --ignored

# Build all guest components (the two *-fixture suites are broken by
# design: runner fixtures for the drift and zero-row hard errors).
build:
    cargo build --target {{wasm_target}} --release \
        -p sample-suite -p provider -p runner-cli -p fixture-suite \
        -p drift-fixture -p zero-gen-fixture -p hang-fixture

# --- verification matrix (AGENTS.md "Build & verify") -----------------
#
# Every path asserts the exit code AND diffs output byte-for-byte
# against expected/ — exit codes alone are blind to verdict flips,
# scheduling breakage, and dropped case events (a module-not-found
# crash exits 1 just like a failing suite does).

# Path 1: wasmtime host-embed runner (tags scheduling + trap path).
verify-embed: build
    #!/usr/bin/env bash
    set -euo pipefail
    ct() { cargo run -q -p component-test-runner --bin ct-runner -- "$@"; }
    out=$(ct {{release_dir}}/sample_suite.wasm) && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-embed-sample.txt <(printf '%s\n' "$out")
    out=$(ct {{release_dir}}/fixture_suite.wasm --missing hsm) && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-embed-fixture.txt <(printf '%s\n' "$out")
    echo "verify-embed: output matches expected/"

# Path 2: composed wasi:cli runner (bundle-then-plug). The JSONL leg
# pins the composed wire format (emitted via the shared results crate)
# and asserts fold equivalence with the host-embed runner byte-for-byte.
verify-compose: build
    #!/usr/bin/env bash
    set -euo pipefail
    tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
    cd examples/compose
    wac compose \
        -d lann:provider-impl=../../{{release_dir}}/provider.wasm \
        -d lann:sample-suite=../../{{release_dir}}/sample_suite.wasm \
        -o bundle.wasm bundle.wac
    wac plug --plug bundle.wasm \
        ../../{{release_dir}}/runner_cli.wasm -o composed.wasm
    out=$(wasmtime run {{wasmtime_flags}} composed.wasm) && code=0 || code=$?
    test "$code" -eq 1
    diff -u ../../expected/verify-run-sample.txt <(printf '%s\n' "$out")
    jsonl=$(wasmtime run {{wasmtime_flags}} --env COMPONENT_TEST_JSONL=1 composed.wasm) \
        && code=0 || code=$?
    test "$code" -eq 1
    cd ../..
    diff -u expected/verify-compose-sample.jsonl <(printf '%s\n' "$jsonl")
    printf '%s\n' "$jsonl" | cargo run -q -p component-test-cli -- fold \
        components/sample-suite/tests.lock > "$tmp/fold.txt" && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-pipeline-sample-fold.txt "$tmp/fold.txt"
    echo "verify-compose: output matches expected/ (incl. JSONL + cross-runner fold)"

# Path 3: jco-node runner (suite transpiled alone; runner-is-provider).
verify-node: build
    #!/usr/bin/env bash
    set -euo pipefail
    cd js/runner-node
    npm install --silent
    npx --yes @bytecodealliance/jco transpile \
        ../../{{release_dir}}/sample_suite.wasm \
        --name suite \
        --async-mode jspi \
        --map 'lann:component-test/test-context@0.1.0=../context.js' \
        -o suite > /dev/null
    out=$(node --experimental-wasm-jspi runner-host-provider.mjs) && code=0 || code=$?
    test "$code" -eq 1
    diff -u ../../expected/verify-run-sample.txt <(printf '%s\n' "$out")
    echo "verify-node: output matches expected/"

# Path 4: inventory + results pipeline (lock check, JSONL fold). The
# fixture leg exercises the JSONL wire shape for trap provenance,
# not-applicable scheduling, and generated rows; the runner's exit is
# captured separately so a mid-stream crash can't hide behind fold's.
verify-pipeline: build
    #!/usr/bin/env bash
    set -euo pipefail
    ct() { cargo run -q -p component-test-runner --bin ct-runner -- "$@"; }
    fold() { cargo run -q -p component-test-cli -- fold "$@"; }
    tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
    ct {{release_dir}}/sample_suite.wasm --jsonl > "$tmp/sample.jsonl" && code=0 || code=$?
    test "$code" -eq 1
    fold components/sample-suite/tests.lock < "$tmp/sample.jsonl" \
        > "$tmp/sample-fold.txt" && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-pipeline-sample-fold.txt "$tmp/sample-fold.txt"
    ct {{release_dir}}/fixture_suite.wasm --jsonl --missing hsm \
        > "$tmp/fixture.jsonl" && code=0 || code=$?
    test "$code" -eq 1
    sed -E 's/"artifact-sha256":"[0-9a-f]{64}"/"artifact-sha256":"<sha256>"/' \
        "$tmp/fixture.jsonl" | diff -u expected/verify-pipeline-fixture.jsonl -
    fold components/fixture-suite/tests.lock < "$tmp/fixture.jsonl" \
        > "$tmp/fixture-fold.txt" && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-pipeline-fixture-fold.txt "$tmp/fixture-fold.txt"
    echo "verify-pipeline: output matches expected/"

# Path 5: cross-target aggregation (examples/aggregate): lock, run the
# fixture against two declared targets, join + validate, diff the
# matrix. The deliberate trap is declared expected-fail (#48), so the
# pipeline aggregates green with the debt reported.
verify-aggregate: build
    #!/usr/bin/env bash
    set -euo pipefail
    ct() { cargo run -q -p component-test-runner --bin ct-runner -- "$@"; }
    cli() { cargo run -q -p component-test-cli -- "$@"; }
    tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
    cli lock {{release_dir}}/fixture_suite.wasm -o "$tmp/tests.lock" > /dev/null
    ct {{release_dir}}/fixture_suite.wasm --jsonl --target native \
        > "$tmp/native.jsonl" && code=0 || code=$?
    test "$code" -eq 1
    ct {{release_dir}}/fixture_suite.wasm --jsonl --target sim --missing hsm \
        > "$tmp/sim.jsonl" && code=0 || code=$?
    test "$code" -eq 1
    cli aggregate --lock "$tmp/tests.lock" \
        --manifest examples/aggregate/targets.toml \
        --results "native=$tmp/native.jsonl" --results "sim=$tmp/sim.jsonl" \
        -o "$tmp/matrix.md" > "$tmp/summary.txt" && code=0 || code=$?
    test "$code" -eq 0  # the deliberate trap is declared expected-fail (#48)
    diff -u expected/verify-aggregate-matrix.md "$tmp/matrix.md"
    echo "verify-aggregate: matrix matches expected/"

# --- lockfiles ---------------------------------------------------------

# Drift-check committed lockfiles against built suites.
lock-check: build
    cargo run -q -p component-test-cli -- lock \
        {{release_dir}}/sample_suite.wasm \
        --check components/sample-suite/tests.lock
    cargo run -q -p component-test-cli -- lock \
        {{release_dir}}/fixture_suite.wasm \
        --check components/fixture-suite/tests.lock

# Regenerate lockfiles after suite changes (commit the diff for review).
lock-update: build
    cargo run -q -p component-test-cli -- lock \
        {{release_dir}}/sample_suite.wasm -o components/sample-suite/tests.lock
    cargo run -q -p component-test-cli -- lock \
        {{release_dir}}/fixture_suite.wasm -o components/fixture-suite/tests.lock

# --- WIT ---------------------------------------------------------------

# Component WIT dirs are symlinks into the canonical copies (wit/ and
# components/provider/wit/provider.wit); there is nothing to sync.

# Validate all WIT trees.
wit-check:
    wasm-tools component wit wit/ > /dev/null
    wasm-tools component wit components/provider/wit > /dev/null
    wasm-tools component wit components/runner-cli/wit > /dev/null
    @echo "all WIT trees valid"
