//! Runner fixture that is broken BY DESIGN — do not "fix" it.
//!
//! One case per hang class, plus containment proof (#45):
//! - `hang/spin`: CPU spin. Wasm never yields, so only the epoch
//!   deadline (the execution budget) can regain control — a host-side
//!   timer cannot fire while the executor thread is stuck in wasm.
//! - `hang/wedge`: async wedge — suspended on a host-originating wait
//!   (a WASI sleep far past any budget). No wasm executes while
//!   suspended, so only the wall-clock case timeout can catch it —
//!   epoch deadline checks never run. (A pure Rust-side
//!   `pending().await` does NOT wedge: wit-bindgen's guest runtime
//!   refuses to park on Rust-originating-only events and traps — see
//!   findings.)
//! - `hang/after`: must pass in a fresh session after both
//!   abandonments (same containment shape as `fixture/trap/after`).
//!
//! Exercised by wasm-gated tests in component-test-runner with
//! budgets of ~1s; with default budgets this suite "just" takes two
//! minutes to fail, so nothing in the golden verify paths runs it.

#[component_test_sdk::suite]
mod hang {
    #[case]
    async fn spin(ctx: &TestContext) -> Verdict {
        ctx.diag("about to spin forever").await;
        #[allow(clippy::empty_loop)]
        loop {
            std::hint::black_box(0u32);
        }
    }

    #[case]
    async fn wedge(ctx: &TestContext) -> Verdict {
        ctx.diag("about to wait on the host for an hour").await;
        std::thread::sleep(std::time::Duration::from_secs(3600));
        Ok(())
    }

    #[case]
    async fn after(ctx: &TestContext) -> Verdict {
        ctx.diag("still alive in a fresh instance").await;
        Ok(())
    }
}
