# component-test GitHub Actions

Composite actions for consumers of the stack (#14), one per concern.
Reference them by pinned rev, like every other consumption path here:
`uses: polymorph-components/polymorph-test/actions/aggregate@<rev>`.

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
  browser-leg fallbacks is not knowable from one data point.
- **publish-viewer** as a *reusable* action: the viewer deploy is a
  workflow in this repository for now (`.github/workflows/pages.yml`);
  generalizing it needs the same second data point.
