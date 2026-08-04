//! UI tests for the `#[suite]` macro's compile-time guards. The
//! compile *errors* are the product here: S1/S3 and the structural
//! lints exist precisely to prevent silently dropped or misdeclared
//! cases, so each rejection is pinned with its message and span.
//! The pass cases prove the expansion (including the wit-bindgen
//! export glue and the tags link-section) compiles natively.
//!
//! .stderr expectations are stable because rust-toolchain.toml pins
//! the compiler. After intentional diagnostic changes:
//! `TRYBUILD=overwrite cargo test -p component-test-sdk --test ui`
//! and review the diff.

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass/*.rs");
    t.compile_fail("tests/ui/fail/*.rs");
}
