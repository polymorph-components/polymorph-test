//! Runner fixture that is broken BY DESIGN — do not "fix" it.
//!
//! The `#[case_row]` fn abuses its raw `&mut Registry` access to
//! register a case *outside* its row prefix, bypassing the static
//! inventory (AGENTS.md: "raw `Registry` registration bypasses
//! inventory and will trip the runner's drift cross-check"). This is
//! the `all()`-only drift direction, which cannot be synthesized by
//! appending section records to a healthy suite; the runner must
//! refuse to run (exit 2), and `tests/runner.rs` in
//! component-test-runner asserts exactly that.

#[component_test_sdk::suite]
mod driftfix {
    use component_test_sdk::{ArcStr, Registry, Tags};

    #[case]
    async fn legit(_ctx: &TestContext) -> Verdict {
        Ok(())
    }

    /// Registers `driftfix/rogue-unrecorded` directly: grammar-valid,
    /// but under no prefix record and matching no exact record.
    #[case_row(prefix = "rows")]
    fn rogue(reg: &mut Registry, _prefix: &ArcStr, _tags: &Tags) {
        reg.case("driftfix/rogue-unrecorded", &[], |_ctx| {
            Box::pin(async { Ok(()) })
        });
    }
}
