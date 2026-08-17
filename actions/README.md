# component-test GitHub Actions

Composite actions for consumers of the stack (#14), one per concern.
Reference them by pinned rev, like every other consumption path here:
`uses: polymorph-components/polymorph-test/actions/aggregate@<rev>`.

## `setup`

Installs the component-test tools at the revision the consumer's
Cargo.lock pins (crate-name-anchored on `component-test-sdk`, so a
renamed repository still resolves), caches the install keyed on
`(os, rev, rust-toolchain.toml)`, and runs the `component-test pins`
gate over every declared lockfile — the one-rev-everywhere check. The
Cargo.lock is the single source of truth; the action cannot be pointed
at a different rev than the workspace builds against, and when the
action itself is referenced by a 40-hex rev, a skewed `uses:` literal
fails the run (branch/tag refs skip with a notice).

```yaml
- uses: polymorph-components/polymorph-test/actions/setup@<rev>
  id: ct
  with:
    cargo-lock: Cargo.lock
    # tools: component-test-cli component-test-runner   (default)
    # install-root: target/ct-tools                     (default)
# steps.ct.outputs.rev — the pinned revision
# steps.ct.outputs.bin — directory holding component-test / ct-runner
```

`source-path: .` installs from a checkout instead of `--git` (used by
this repository's own CI smoke, and the local-override analogue).

The installed CLI carries the whole composition/execution surface —
`wizen` (pre-initialize large suites, #25/#85), `compose-runner`, and
`run` (embedded reference provider, runner core, and wasmtime) — so a
consumer pipeline that only needs the composed path installs no wac
and no wasmtime. The embedded components are built from source by the
CLI's build script, so installing with default features needs the
`wasm32-wasip2` target (already in the toolchain of every repo that
builds suites; measured cost ~zero — the wasm builds in the shadow of
the CLI's own compile). A host-only CLI for wasm-free contexts
installs with `--no-default-features`: `compose-runner`/`run` then
require explicit `--runner`/`--provider`, everything else is
unchanged (the aggregate action's fallback install below uses exactly
this). Wizening in CI is one line after setup, on the built artifact:

```sh
target/ct-tools/bin/component-test wizen suite.wasm -o suite.wasm
```

(Run the wizened artifact everywhere downstream — runners and
`lock --check` alike; mixing artifacts is the failure mode.) There is
deliberately no `wizen:` input on this action: setup runs before the
consumer's suite artifacts exist. When a run-suite action materializes
(below), wizening belongs there as an input.

### The local half: one canonical `_ct-tools` recipe

CI is only half the bootstrap; local `just` runs need the same tools.
Consumers should carry exactly this recipe (module-path-adjusted), so
the copies diff empty against each other:

```just
# Install the component-test CLI and host runner at the revision the
# workspace pins (read from Cargo.lock, so a bump cannot half-apply),
# then hold every other appearance of the pin to it.
_ct-tools:
    #!/usr/bin/env bash
    set -euo pipefail
    cd {{root}}
    src=$(grep -m1 -A2 '^name = "component-test-sdk"' Cargo.lock | grep '^source = "git+')
    url=$(printf '%s' "$src" | sed -E 's/^source = "git\+([^?]+)\?.*/\1/')
    rev=$(printf '%s' "$src" | grep -oE '[0-9a-f]{40}' | head -n1)
    if [ ! -x target/ct-tools/bin/component-test ] || [ "$(cat target/ct-tools/.rev 2>/dev/null)" != "$rev" ]; then
        cargo install --locked --root target/ct-tools --git "$url" --rev "$rev" component-test-cli component-test-runner
        printf '%s' "$rev" > target/ct-tools/.rev
    fi
    target/ct-tools/bin/component-test pins --cargo-lock Cargo.lock --expect "$rev" > /dev/null
```

(Drop `component-test-runner` if the runner is embedded as a library.
The JS runner core is consumed from JSR as `@polymorph/test`, pinned by
version in the consumer's lockfiles and gated by the consumer's own
runner-js pin check — the cargo rev is the only pin this gate holds.)

### Transpile stamps

Consumers guard their jco transpiles with a content stamp so `just`
runs skip redundant work. The stamp must cover the suite artifacts
**and the jco tree's `package.json`**: the transpile flags and the
pinned transpiler both live there, and either changing must invalidate
the generated tree — a stamp keyed on the wasm alone has demonstrably
shipped stale output across a flag change.

```just
stamp=$(cat "{{suite}}" jco/package.json | sha256sum | cut -d' ' -f1)
if [ "$(cat jco/generated/.stamp 2>/dev/null || true)" != "$stamp" ]; then
    (cd jco && npm run --silent transpile)
    printf '%s' "$stamp" > jco/generated/.stamp
fi
```

## `aggregate`

Validates per-target results-JSONL against a lockfile + target
manifest, publishes the matrix to the job summary, and turns the CLI's
findings into workflow annotations (`warning:` lines → `::warning`,
`error:` lines → `::error`). The verdict is the CLI's own exit code,
passed through unchanged — a missing *optional* target annotates and
stays green; a failing case, coverage gap, or manifest violation is
red. The action adds presentation, never verdict.

```yaml
- uses: polymorph-components/polymorph-test/actions/aggregate@<rev>
  with:
    lock: conformance/tests.lock
    manifest: conformance/targets.toml
    results: |
      wasmtime=results/wasmtime.jsonl
      jco-node=results/jco-node.jsonl
    summary-title: Conformance matrix
    # Optional: a prebuilt binary. Without it the action
    # `cargo install --locked`s the CLI from this repo at the same
    # ref the action was referenced by (presentation and validation
    # semantics cannot skew).
    cli: target/ct-tools/bin/component-test
    # Optional but recommended: with the consumer's Cargo.lock, the
    # action fails when the `uses:` rev above skews from the lock's
    # component-test pin — replacing the hand-rolled workflow grep
    # guard. Branch/tag refs skip the check with a notice.
    cargo-lock: Cargo.lock
```

## Not here (yet)

- **run-suite** (execute a suite artifact on a runner kind, upload
  results): deliberately parked until a second consumer exists — the
  generic/specific line for SUT-linked drivers, artifact caches, and
  browser-leg fallbacks is not knowable from one data point. #85's
  `wizen: true` flag lands here as an input when it exists.
- **publish-viewer** as a *reusable* action: the viewer deploy is a
  workflow in this repository for now (`.github/workflows/pages.yml`);
  generalizing it needs the same second data point.
