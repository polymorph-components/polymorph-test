# Writing a suite

The complete recipe for a new test-suite component. `src/lib.rs` in
this directory is the living example; `../fixture-suite` additionally
shows tag inheritance, a polarity flip, and a generated row.

## Crate setup

```toml
[package]
name = "my-suite"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
component-test-sdk = { git = "..." } # or path/version
```

That is the whole manifest: no `wit/` directory, no wit-bindgen — the
SDK owns the contract bindings and the `#[suite]` macro emits all
glue. (A suite that *imports* a system-under-test adds its own
`wit-bindgen` and a plain `wit_bindgen::generate!` for that surface
only.)

## Cases

```rust
#[component_test_sdk::suite]
mod my_suite {
    mod group {
        #[case]
        async fn works(ctx: &TestContext) -> Verdict {
            ctx.diag("probing").await;        // diagnostics sideband
            check_eq!(2 + 2, 4, "2 + 2");     // (actual, expected)
            let n: i32 = "7".parse().or_fail()?; // any Display error
            check!(n > 0);
            Ok(())
        }
    }
}
```

Case names derive from the module path + fn name (`my-suite/group/works`;
idents map `_` → `-`). `Ok(())` = pass, `failed(...)`/`skipped(...)` =
explicit verdicts, `?` works via `From<E: Error>` or `.or_fail()` for
Display-only errors (anyhow). Feature tags (`#[case(tags("hsm"))]`,
`#[cases(tags(...))]` on a module) gate cases against target capability
manifests — every positively-tagged feature needs a `!feature` decline
case (compile-time lint). Dynamic corpora go through
`#[case_generator(prefix = "...")]` — see the `#[suite]` rustdoc.

## Build, lock, run

```sh
cargo build --target wasm32-wasip2 --release
component-test lock target/wasm32-wasip2/release/my_suite.wasm -o tests.lock
ct-runner target/wasm32-wasip2/release/my_suite.wasm            # or --jsonl
```

Commit `tests.lock` and regenerate it (`lock ... -o`) after any case
change: the diff is the review surface, and runners/aggregation
cross-check the inventory against what the suite actually enumerates.

## Large suites

Enumeration builds the whole registry per fresh instance, so
instance-per-case isolation gets expensive at scale (10k cases ≈ 3ms
per instance). Instead of relaxing isolation, pre-initialize the
artifact once after building:

```sh
component-test wizen target/wasm32-wasip2/release/my_suite.wasm -o my_suite.wasm
```

The suite needs no changes (the contract's own `all()` is the init
function), inventory and tag scheduling survive, and every fresh
instance is born with the registry built — a 10k-case synthetic's
K=1 run went 30.8s → 7.1s sequential, 1.14s at `--jobs 8`
(findings.md 22–24; JS legs benefit far less — no copy-on-write
memory images). Two caveats: the
snapshot freezes whatever init observed (env, entropy, clocks), and
the wizened artifact must be the one used everywhere downstream
(runners, `lock --check`). Suites that import a system-under-test
drive `component_test_runner::wizen::wizen_with` with their own
linker instead.
