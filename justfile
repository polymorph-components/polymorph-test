# component-test task runner. `just` lists recipes; `just all` is the
# full verification matrix from AGENTS.md. CI job bodies live in the gha
# module (`.github/justfile`); `just ci` mirrors the gating CI jobs.

wasm_target := "wasm32-wasip2"
release_dir := "target" / wasm_target / "release"
wasmtime_flags := "-W component-model-async -S p3"

# GitHub Actions plumbing: CI job entry points.
mod gha '.github'

# List the available recipes.
default:
    @just --list --unsorted

# The exact set of checks CI runs: each gating CI job runs exactly one
# gha:: job recipe. The actions-setup-smoke job is excluded: it tests
# the actions/setup composite action and only runs under Actions.
ci: (gha::host-checks) (gha::verify)

# Everything: host tests, component builds, all verification paths.
all: build test test-wasm lock-check verify-embed verify-compose verify-cli verify-deltic verify-pipeline verify-aggregate verify-viewer verify-imports verify-emit
# The fast pre-commit checks: formatting, clippy, host tests, WIT
# validation. The CI job of the same name runs the identical set
# through gha::host-checks.
check: fmt-check lint test wit-check

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
        -d polymorph:provider-impl=../../{{release_dir}}/provider.wasm \
        -d polymorph:sample-suite=../../{{release_dir}}/sample_suite.wasm \
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

# Path 2b: the CLI's composition/execution subcommands (#85).
# compose-runner (embedded provider + runner core, built from source
# by the CLI's build.rs — always current, #88) must reproduce Path 2's
# goldens under the wasmtime CLI; run is the same composition under
# the embedded wasmtime (human + JSONL legs); wizen pre-initializes
# with inventory, scheduling, and runnability intact (findings 22–24).
verify-cli: build
    #!/usr/bin/env bash
    set -euo pipefail
    cli() { cargo run -q -p component-test-cli -- "$@"; }
    ct() { cargo run -q -p component-test-runner --bin ct-runner -- "$@"; }
    tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
    cli compose-runner {{release_dir}}/sample_suite.wasm -o "$tmp/composed.wasm"
    out=$(wasmtime run {{wasmtime_flags}} "$tmp/composed.wasm") && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-run-sample.txt <(printf '%s\n' "$out")
    out=$(cli run {{release_dir}}/sample_suite.wasm) && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-run-sample.txt <(printf '%s\n' "$out")
    jsonl=$(cli run --jsonl {{release_dir}}/sample_suite.wasm) && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-compose-sample.jsonl <(printf '%s\n' "$jsonl")
    cli wizen {{release_dir}}/sample_suite.wasm -o "$tmp/wizened.wasm"
    cli lock "$tmp/wizened.wasm" --check components/sample-suite/tests.lock
    out=$(ct "$tmp/wizened.wasm") && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-embed-sample.txt <(printf '%s\n' "$out")
    out=$(cli run "$tmp/wizened.wasm") && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-run-sample.txt <(printf '%s\n' "$out")
    echo "verify-cli: output matches expected/ (compose-runner, run, wizen)"

# The one-version-everywhere gate for the deltic pin (successor to the
# retired fetch script's `assertPinConsistency`): every jsr:@deltic/*
# specifier in every deno.json, and every @deltic package the lock
# resolves, must name the SAME prerelease — one version names one
# upstream commit, and the browser assets are built from that same graph.
#
# CONTRACT: as of deltic pre-b297834 (A10), @deltic/protocol ships as a
# stable, independently-versioned package (0.1.0, no per-commit prerelease
# hash) that @deltic/{runtime,wasi} depend on transitively — it is not
# named by any jsr:@deltic/*@<hash> specifier in deno.json and isn't part
# of the "one version names one commit" pin this gate polices. Excluded by
# name rather than by pattern, so drift in any *other* @deltic package
# still trips the gate. See contracts/embedder-api.md "Migration notes"
# (upstream, pre-b297834) and js/runner-deltic/deno.lock.
deltic-pin-gate:
    #!/usr/bin/env bash
    set -euo pipefail
    configs=(js/runner-deltic/deno.json)
    v=$(grep -ho 'jsr:@deltic/[a-z-]*@[^/"]*' "${configs[@]}" | sed 's/.*@//' | sort -u)
    test -n "$v" || { echo "deltic pin gate: no jsr:@deltic specifiers found" >&2; exit 1; }
    [ "$(printf '%s\n' "$v" | wc -l)" = 1 ] || { echo "deltic pin drift: $v" >&2; exit 1; }
    python3 - "$v" js/runner-deltic/deno.lock <<'PY'
    import json, sys
    want, lock = sys.argv[1], json.load(open(sys.argv[2]))
    stable = {"@deltic/protocol"}  # see CONTRACT note above deltic-pin-gate
    def pkg_name(k):
        # k looks like "jsr:@deltic/foo@1.2.3" or "@deltic/foo@1.2.3"
        rest = k[len("jsr:"):] if k.startswith("jsr:") else k
        return rest.rsplit("@", 1)[0]
    bad = {k: r for k, r in lock.get("specifiers", {}).items()
           if "@deltic/" in k and r != want and pkg_name(k) not in stable}
    bad.update({k: k for k in lock.get("jsr", {}) if k.startswith("@deltic/")
                and not k.endswith("@" + want) and pkg_name(k) not in stable})
    if bad:
        sys.exit(f"deltic lock drift (want {want}): {sorted(bad)}")
    PY
    echo "deltic pin gate: all @deltic packages pinned to $v (protocol excluded, stable)"

# The deltic browser-leg assets, built from the pinned JSR graph (no
# network fetch, no sha bookkeeping — deno.lock carries JSR package
# integrity and --frozen enforces it):
#
#   target/deltic-browser/deltic-embedder.mjs         bundled from
#       js/runner-deltic/browser-bundle-entry.ts (embedder API +
#       Translator + ct-runner glue + wasi shims, one platform-neutral
#       ES module — what the browser worker and the node selftest load)
#   target/deltic-browser/deltic-translator-shim.wasm the translator
#       asset packaged in @deltic/translator, copied out of the
#       lock-pinned module cache (the Deno runner leg needs no copy: it
#       loads the packaged translator through the module graph)
#
# The directory is version-free on purpose: the lock owns versioning.
deltic-assets: deltic-pin-gate
    #!/usr/bin/env bash
    set -euo pipefail
    out=target/deltic-browser
    mkdir -p "$out"
    deno bundle --config js/runner-deltic/deno.json --frozen --platform browser \
        -o "$out/deltic-embedder.mjs" js/runner-deltic/browser-bundle-entry.ts
    tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
    deno info --json --config js/runner-deltic/deno.json --frozen @deltic/translator \
        > "$tmp/info.json"
    pin=$(grep -o 'jsr:@deltic/runtime@[^/"]*' js/runner-deltic/deno.json | head -1 | sed 's/.*@//')
    python3 - "$tmp/info.json" "$out/deltic-translator-shim.wasm" "$pin" <<'PY'
    import json, sys
    graph = json.load(open(sys.argv[1]))
    want = sys.argv[3]
    mods = [m for m in graph["modules"] if "/@deltic/" in m.get("specifier", "")]
    bad = {m["specifier"] for m in mods if want not in m["specifier"]}
    if bad:
        sys.exit(f"pin drift in translator graph (expected {want}): {bad}")
    asset = next(m for m in mods if m["specifier"].endswith("/translator_shim.wasm"))
    # CONTRACT: the migration contract copies the cache file wholesale, but
    # Deno 2.9.5 stores remote modules as body + a "\n// denoCacheMetadata={...}"
    # trailer; copying it verbatim yields a wasm that fails to compile
    # ("section out of order"). Take exactly the module's own byte length
    # (deno info's `size`, == the response content-length) and assert that
    # anything past it is only that trailer.
    blob = open(asset["local"], "rb").read()
    body, rest = blob[: asset["size"]], blob[asset["size"] :]
    if not body.startswith(b"\0asm"):
        sys.exit("translator asset is not a wasm module")
    if rest and not rest.lstrip(b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\t\n\r ").startswith(b"// denoCacheMetadata="):
        sys.exit("unexpected trailing bytes in cached translator asset")
    open(sys.argv[2], "wb").write(body)
    PY

# Path 3: deltic-deno runner (runner-is-provider, like Path 3, but no
# transpile step, no generated tree, and no engine flag — deltic is a
# runtime linker; the contract's async exports run on the callback ABI
# under stock Deno). Pinned to exact deltic JSR prereleases by
# js/runner-deltic/deno.json, with deno.lock enforced via --frozen and
# agreement asserted by deltic-pin-gate. The Deno leg's translator comes
# from the packaged @deltic/translator through the module graph; the
# browser/node legs use the repo-built assets (`just deltic-assets`).
# Tag scheduling comes from the suite's own embedded
# inventory (deltic#25): the fixture leg runs --missing hsm exactly like
# Paths 1/4 and schedules the hsm case out as not-applicable; the sample
# legs reuse Path 2/3's human golden and Path 2/4's fold golden. The
# selftest leg drives the BROWSER worker's engine path (js/runner-deltic/
# engine.mjs + the shared harness.mjs case loop) over the pinned embedder
# bundle under plain node — no --experimental-wasm-jspi: the callback ABI
# needs no engine flag, which is the browser-leg premise.
verify-deltic: build deltic-assets
    #!/usr/bin/env bash
    set -euo pipefail
    tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
    translator=target/deltic-browser/deltic-translator-shim.wasm
    bundle=target/deltic-browser/deltic-embedder.mjs
    run() { deno run --allow-read=target --config js/runner-deltic/deno.json --frozen \
        js/runner-deltic/runner.ts "$@"; }
    norm() { sed -E -e 's/"artifact-sha256":"[0-9a-f]{64}"/"artifact-sha256":"<sha256>"/' \
        -e 's/,"duration-ms":[0-9]+//g'; }
    fold() { cargo run -q -p component-test-cli -- fold "$@"; }
    out=$(run {{release_dir}}/sample_suite.wasm) && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-run-sample.txt <(printf '%s\n' "$out")
    run {{release_dir}}/sample_suite.wasm --jsonl > "$tmp/sample.jsonl" && code=0 || code=$?
    test "$code" -eq 1
    norm < "$tmp/sample.jsonl" | diff -u expected/verify-deltic-sample.jsonl -
    fold components/sample-suite/tests.lock < "$tmp/sample.jsonl" \
        > "$tmp/sample-fold.txt" && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-pipeline-sample-fold.txt "$tmp/sample-fold.txt"
    run {{release_dir}}/fixture_suite.wasm --jsonl --missing hsm \
        > "$tmp/fixture.jsonl" && code=0 || code=$?
    test "$code" -eq 1
    norm < "$tmp/fixture.jsonl" | diff -u expected/verify-deltic-fixture.jsonl -
    fold components/fixture-suite/tests.lock < "$tmp/fixture.jsonl" \
        > "$tmp/fixture-fold.txt" && code=0 || code=$?
    test "$code" -eq 1
    diff -u expected/verify-deltic-fixture-fold.txt "$tmp/fixture-fold.txt"
    node js/runner-deltic/selftest.mjs "$bundle" "$translator" \
        {{release_dir}}/sample_suite.wasm {{release_dir}}/fixture_suite.wasm
    echo "verify-deltic: output matches expected/ (incl. shared human + fold goldens)"

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

# The JUnit emitter (#11): golden-diff the fixture pipeline's XML
# (times normalized - durations vary per run; everything else is
# byte-stable).
verify-emit: build
    #!/usr/bin/env bash
    set -euo pipefail
    ct() { cargo run -q -p component-test-runner --bin ct-runner -- "$@"; }
    cli() { cargo run -q -p component-test-cli -- "$@"; }
    tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
    ct {{release_dir}}/fixture_suite.wasm --jsonl --target native \
        > "$tmp/native.jsonl" && code=0 || code=$?
    test "$code" -eq 1
    ct {{release_dir}}/fixture_suite.wasm --jsonl --target sim --missing hsm \
        > "$tmp/sim.jsonl" && code=0 || code=$?
    test "$code" -eq 1
    cli emit junit --results "native=$tmp/native.jsonl" --results "sim=$tmp/sim.jsonl" \
        | sed -E 's/time="[0-9.]+"/time="0.000"/g' > "$tmp/results.xml"
    diff -u expected/verify-emit-junit.xml "$tmp/results.xml"
    echo "verify-emit: junit matches expected/"

# The fixture pipeline's JUnit XML at a stable path: CI's reporter
# step renders it into the job summary (the #11 demo). The deliberate
# trap is the point - the summary shows a real failure row.
emit-demo: build
    #!/usr/bin/env bash
    set -euo pipefail
    ct() { cargo run -q -p component-test-runner --bin ct-runner -- "$@"; }
    cargo run -q -p component-test-cli -- lock {{release_dir}}/fixture_suite.wasm \
        -o target/tests.lock > /dev/null
    ct {{release_dir}}/fixture_suite.wasm --jsonl --target native \
        > target/native.jsonl || true
    ct {{release_dir}}/fixture_suite.wasm --jsonl --target sim --missing hsm \
        > target/sim.jsonl || true
    cargo run -q -p component-test-cli -- emit junit \
        --results native=target/native.jsonl --results sim=target/sim.jsonl \
        -o target/junit-demo.xml
    echo "wrote target/junit-demo.xml"

# --- viewer ------------------------------------------------------------

# Build the viewer's engines: the viewer-aggregate component (the gate's
# aggregation compiled to wasm — runtime-linked by deltic in the page,
# no transpile step) and the demo suites (component wasm, verbatim),
# plus the pinned deltic assets copied beside the viewer so local serve
# and Pages share one relative layout (js/viewer/deltic.mjs).
viewer-build: build deltic-assets
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release --target wasm32-wasip2 -p viewer-aggregate
    mkdir -p js/viewer/deltic js/viewer/generated js/viewer/suite
    cp target/deltic-browser/deltic-translator-shim.wasm \
        target/deltic-browser/deltic-embedder.mjs js/viewer/deltic/
    cp {{release_dir}}/viewer_aggregate.wasm js/viewer/generated/viewer-aggregate.wasm
    cp {{release_dir}}/sample_suite.wasm {{release_dir}}/fixture_suite.wasm js/viewer/suite/

# The viewer's drift gate: the wasm aggregate under deltic (exactly the
# page's instantiation) must reproduce the CLI gate's verdicts over the
# fixture pipeline, plus the shared harness loop's gating adapters over
# synthetic cases. Plain node — no JSPI flag anywhere. The suite-
# execution legs live in verify-deltic's selftest. The page itself is
# thin glue over these engines plus worker plumbing.
verify-viewer: viewer-build
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
    node js/viewer/selftest.mjs \
        "$tmp/tests.lock" examples/aggregate/targets.toml \
        "$tmp/native.jsonl" "$tmp/sim.jsonl" \
        js/viewer/deltic/deltic-embedder.mjs \
        js/viewer/deltic/deltic-translator-shim.wasm \
        js/viewer/generated/viewer-aggregate.wasm

# The shared consumer glue (import binding, envelope normalization,
# the suite-runner loop, the node driver helpers): plain node, no wasm.
verify-imports:
    node js/viewer/imports.test.mjs
    node js/node-runner.test.mjs
    node js/browser-driver.test.mjs
    node --check js/viewer/browser-worker.mjs
    node --check js/viewer/page-runner.mjs

# Serve the viewer over the repository root (demo fixtures + transpiled
# suites resolve by relative path): http://127.0.0.1:8123/
viewer-serve: viewer-build
    node js/viewer/serve.mjs

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
