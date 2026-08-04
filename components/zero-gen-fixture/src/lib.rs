//! Runner fixture that is broken BY DESIGN — do not "fix" it.
//!
//! The `phantom` feature's decline coverage exists only as a zero-row
//! generator's prefix record: enough to satisfy the macro's *static*
//! decline-pair lint (the `!phantom` prefix record pairs the positive
//! tag), but no `!phantom` case ever materializes. The runner's
//! materialized decline-pair check must refuse to run this suite
//! (exit 2), and `tests/runner.rs` in component-test-runner asserts
//! exactly that.

#[component_test_sdk::suite]
mod zerogen {
    #[case(tags("phantom"))]
    async fn uses_phantom(_ctx: &TestContext) -> Verdict {
        Ok(())
    }

    /// Zero rows: vacuously satisfies the static lint, provides no
    /// actual decline coverage.
    #[case_generator(prefix = "declines", tags("!phantom"))]
    fn rows() -> impl Iterator<Item = Case> {
        std::iter::empty()
    }
}
