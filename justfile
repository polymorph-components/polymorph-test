# component-test task runner. `just` lists recipes; `just all` is the
# full verification matrix from AGENTS.md.

wasm_target := "wasm32-wasip2"
release_dir := "target" / wasm_target / "release"
wasmtime_flags := "-W component-model-async -S p3"

_default:
    @just --list --unsorted

# Everything: host tests, component builds, all four verification paths.
all: test build lock-check verify-embed verify-compose verify-node verify-pipeline

# CI's native job: formatting, clippy, host tests, WIT validation.
host-checks: fmt-check lint test wit-check

fmt-check:
    cargo fmt --all --check

lint:
    cargo clippy --workspace \
        --exclude sample-suite --exclude provider \
        --exclude runner-cli --exclude fixture-suite \
        --all-targets -- -D warnings

# Host-crate tests (fast; excludes wasm-only component crates).
test:
    cargo test --workspace \
        --exclude sample-suite --exclude provider \
        --exclude runner-cli --exclude fixture-suite

# Build all guest components.
build:
    cargo build --target {{wasm_target}} --release \
        -p sample-suite -p provider -p runner-cli -p fixture-suite

# --- verification matrix (AGENTS.md "Build & verify") -----------------

# Path 1: wasmtime host-embed runner (tags scheduling + trap path).
verify-embed: build
    cargo run -q -p component-test-runner --bin ct-runner -- \
        {{release_dir}}/sample_suite.wasm; test $? -eq 1
    cargo run -q -p component-test-runner --bin ct-runner -- \
        {{release_dir}}/fixture_suite.wasm --missing hsm; test $? -eq 1

# Path 2: composed wasi:cli runner (bundle-then-plug).
verify-compose: build
    cd examples/compose && wac compose \
        -d lann:provider-impl=../../{{release_dir}}/provider.wasm \
        -d lann:sample-suite=../../{{release_dir}}/sample_suite.wasm \
        -o bundle.wasm bundle.wac
    cd examples/compose && wac plug --plug bundle.wasm \
        ../../{{release_dir}}/runner_cli.wasm -o composed.wasm
    cd examples/compose && wasmtime run {{wasmtime_flags}} composed.wasm; \
        test $? -eq 1

# Path 3: jco-node runner (suite transpiled alone; runner-is-provider).
verify-node: build
    cd js/runner-node && npm install --silent
    cd js/runner-node && npx --yes @bytecodealliance/jco transpile \
        ../../{{release_dir}}/sample_suite.wasm \
        --async-mode jspi \
        --map 'lann:component-test/test-context@0.1.0=../context.js' \
        -o suite > /dev/null
    cd js/runner-node && node --experimental-wasm-jspi \
        runner-host-provider.mjs; test $? -eq 1

# Path 4: inventory + results pipeline (lock check, JSONL fold).
verify-pipeline: build
    cargo run -q -p component-test-runner --bin ct-runner -- \
        {{release_dir}}/sample_suite.wasm --jsonl \
        | cargo run -q -p component-test-cli -- fold \
            components/sample-suite/tests.lock; test $? -eq 1

# --- lockfiles ---------------------------------------------------------

# Drift-check committed lockfiles against built suites.
lock-check: build
    cargo run -q -p component-test-cli -- lock \
        {{release_dir}}/sample_suite.wasm \
        --check components/sample-suite/tests.lock
    cargo run -q -p component-test-cli -- lock \
        {{release_dir}}/fixture_suite.wasm \
        --check components/fixture-suite/tests.lock

# Regenerate lockfiles after suite changes (commit the diff — the diff
# is the review surface).
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
