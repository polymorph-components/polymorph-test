# Runner execution policy

Guidance for L3 runner implementers and for consumers tuning the
reference runners' knobs. Nothing here is contract: the L1 WIT surface
(`wit/tests.wit`) constrains none of it, and both instance
granularities below are legal implementations of it. Where this
document and a design commitment in the README disagree, the README
wins. The numbers come from two measurement campaigns: the
webcrypto-conformance corpus (11,578 generated cases, 17-core linux,
issue #22's thread) and a 10k-case synthetic
(`components/bench-suite`, findings.md 19–24).

Most of this policy ships as *defaults* in the reference wasmtime
runner (`crates/component-test-runner`) rather than advice; the
sections say which knob, if any, moves each behavior.

## Instance granularity: instance-per-case is the default

A fresh instance per case (`cases_per_instance = 1`, ct-runner's
default) buys, in one mechanism:

- **isolation as mechanical fact** — no shared guest state exists to
  leak, so case self-containment stops being a discipline;
- **trivial trap containment** — a trap kills one case's store: record
  a trap-provenance `fail`, instantiate the next case. No resume
  protocol, no case-trap vs harness-error distinction, no
  re-instantiation loops to guard against;
- **replication as the parallelism axis** (see below) with sharding
  granularity 1.

The knob (`--cases-per-instance`): `1` = per-case; `K` = a fresh
instance every K cases; `0` = one instance for the whole run (or per
worker, under parallelism). A trap abandons the current instance
regardless of K — the next case gets a fresh session, resumed
positionally at the next census index (deterministic `all()` order
across instances is a contract guarantee; the JS harness's positional
relocation additionally name-verifies, finding 21). The fold
synthesizes `not-reached` only for cases an abandoned *run* never got
to. Diagnostics already emitted for an abandoned case stand, marked as
a prefix (`diagnostics-complete: false`).

Shared-instance modes (K > 1, K = 0) are the documented fallback for
targets where per-case setup dominates — browser boots are the
canonical example, and load-induced per-case boot timeouts are their
flake signature. The cost is that poisoning forfeits instance state
for the remainder of the recycle window; the reference runner's
session-slot machinery (poison + re-create + resume by index) makes
that a per-case trap rather than a run failure.

### What the default costs, measured

Profiling the 11.5k-case corpus under K=1: **95% of wall time was
`all()`** — per-instance registry construction plus lifting 11.5k
handles (6.9 ms/case) — while instantiation was 64 µs (copy-on-write
images; the pooling allocator added no measurable win), case work
358 µs, teardown 57 µs. Enumeration, not instantiation, is the K=1 tax.

The mitigation ladder, each step removing its own layer (full-run wall
clock, same corpus):

| change | full run |
|---|---|
| baseline: JSON corpus parsed per instance, K=1, sequential | 11:36 |
| build-time corpus preprocessing (postcard) | 3:35 |
| zero-copy corpus (rkyv `access_unchecked`) | 1:23 |
| K=1 sequential, all of the above | 0:36 |
| **K=1, `--jobs 8` — full per-case isolation** | **7.3s** |
| K=0, `--jobs 8` — instance-per-worker | 2.3s |
| (incumbent single-instance concurrent adapter, for scale) | (4.3s) |

Doctrine: make registry construction cheap *first* (preprocess corpora
at build time; keep construction O(cases served), not O(suite) — see
findings 19–21 for the quadratic traps). After that, full isolation
runs within ~1.7× of the incumbent single-instance concurrent adapter
it replaced (7.3s vs 4.3s) and ~3× of the fastest shared-instance
replication mode (K=0, `--jobs 8`: 2.3s) — a price worth paying by
default. Tune K high or 0 only for cheap pure-compute corpora where
even that multiple matters, or for targets whose per-instance boot is
the dominant term.

### K=1 at scale: wizen instead of relaxing isolation

Wizer pre-initialization (`component-test wizen`, #25/#85) runs the
suite's own `all()` at build time and snapshots the heap: every fresh
instance is born with the registry built, removing the enumeration tax
without giving up isolation. On the 10k synthetic under the wasmtime
runner: per-instance `all()` 3.15 ms → 663 µs; instantiation unchanged
(~19 µs — CoW absorbs the 122 KB → 1.29 MB artifact growth); K=1
full-isolation run 30.8s → 7.1s sequential, **1.14s at `--jobs 8`**.
Inventory, tag scheduling, and `lock --check` all work unchanged on
the wizened artifact (finding 22).

When it does *not* pay:

- **JS legs (deltic, browser): net ~1.5× at best.** V8 has no
  copy-on-write memory images, so the 1.29 MB active data segment is
  copied at every instantiation (0.78 ms → 2.17 ms), eating most of
  the enumeration win (finding 24). Instance granularity K>1 remains
  the JS-leg lever.
- **K=0 topologies: irrelevant.** The registry builds once (~12 ms on
  the 11.5k corpus with a preprocessed corpus); there is nothing left
  to amortize.

Caveats the wizened artifact carries: the snapshot freezes whatever
init observed (env, entropy, clocks — baked at wizen time for every
future instance), and the wizened artifact must be the one used
everywhere downstream — runners, lockfile checks, hashing — never
mixed with the unwizened build. Suites with SUT imports drive
`component_test_runner::wizen::wizen_with` with their own linker; see
`components/sample-suite/README.md` for the suite-author view.

## Parallelism: replication, striping, byte-stable output

Intra-instance concurrency is cooperative-only under the component
model, so **replication is the parallelism axis**: `--jobs N` workers
share the compiled engine, own their stores, and each runs the modulo
stripe `runnable-index % jobs == worker`. Striping, not contiguous
chunking: expensive cases cluster by construction in generated corpora
(by algorithm, by key size), so chunks systematically imbalance.
`cases_per_instance` applies per worker; K=0 under parallelism means
instance-per-worker — the replication doctrine literally.

Results are emitted in census order regardless of completion order, so
parallel runs produce byte-identical result streams — parallelism is
invisible to everything downstream (goldens, folds, aggregation).
Under `--jobs`, diagnostics print with their case's block rather than
live.

Parallelism multipliers need empirical tuning per target class:
browser targets saturate at low multiples, and load-induced timeouts
are the flake signature to watch for when raising them.

## Layered guards: execution budgets and wall timeouts

Two guards per case, catching disjoint failure modes
(`--case-execution-budget`, default 10s; `--case-timeout`, default
120s; `0` disables either):

- The **execution budget** meters actual wasm *execution* — the
  executing thread's CPU time, sampled at epoch ticks — so it catches
  CPU spins, which no wall timer can (the executor thread is stuck
  inside wasm), and a contended machine stretching wall clock does not
  eat a case's budget.
- The **wall timeout** covers suspension, so it catches async wedges —
  a case awaiting something that never resolves — which the execution
  budget cannot see (no wasm runs while wedged).

Either trip fails the case with provenance `limit-exceeded(<kind>)`
and abandons the instance: the same containment as a trap, and the
next case gets a fresh session. Layer any SUT-internal operation
timeout *below* the runner's guards, so genuine failures classify as
`failed` outcomes with their own diagnostics and only true hangs trip
the guard. Emit phase markers in diagnostics so an abandoned case's
prefix identifies the hung phase. (Budget escalation — a keepalive
that lets a legitimately slow case extend its lease — is #47's
territory.)

## No retries

No execution path retries anything, and there is deliberately no knob:
a second attempt masks exactly the flake a test system exists to
report. For network and timing nondeterminism, prefer deterministic
simulation environments over retries.

## Reporting obligations

- **Write-through**: forward diagnostics to the transport as they
  arrive. Anything buffered inside a wasm runner core dies with the
  store on a trap; the composed wasi:cli core streams stdout
  write-through for exactly this reason.
- **Concurrent drain**: a runner observing a case's diagnostics stream
  must consume it concurrently with `run` — a non-draining observer
  wedges sync-lifted suites (finding 3) — and must drain biased,
  taking completed messages before the verdict, or in-flight
  diagnostics are lost with the future (finding 5).
- **Census truth**: one event per census case, in census order, ending
  with the terminator. Cases the run never reached are the fold's to
  synthesize (`not-reached`), not the runner's to invent.

## Selection is not capability

Two different absences, never conflated:

- **`not-applicable`** is *capability*: the target's manifest declares
  a feature missing, and tag scheduling excludes the case. It is a
  property of the target and is reported truthfully regardless of any
  filter.
- **`deselected`** is *selection policy*: cost tiers, smoke subsets on
  expensive targets, a dev loop's `--only`. The runner reports the
  unselected remainder of the census as `deselected` (never executed,
  no provenance), so subset runs still fold and aggregate cleanly with
  the subsetting visible in the results — restricting a
  browser-boot-per-case target to a smoke tier is policy someone chose,
  and the matrix shows it as such.

Capability takes precedence: a tags-excluded case stays
`not-applicable` even when it also falls outside the selection —
aggregation's applicability policing rejects `deselected` where the
manifest says the case could never have run (selection must not hide
capability). An empty selection (`--only` matching nothing) is a run
error, not a vacuous green. (The JS leg's `only` option predates this
rule and still omits filtered cases instead of reporting them —
tracked in #89.)
