//! Sample test suite, now on the SDK: registry-driven cases with thin
//! generated-bindings glue. A passing case, a failing case, and a
//! runtime-skipped case (the exceptional escape hatch), all emitting
//! diagnostics.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "suite",
        generate_all,
    });
}

use std::cell::OnceCell;

use bindings::exports::lann::component_test::tests::{Guest, GuestTestCase, Outcome, TestCase};
use bindings::lann::component_test::test_context::Context;
use component_test_sdk::{case, check, check_eq, skipped, Registry, Verdict};

// ---------------------------------------------------------------- cases

async fn math_add(ctx: &Context) -> Verdict {
    ctx.diagnostic("computing 2 + 2".into()).await;
    let got = 2 + 2;
    ctx.diagnostic(format!("got {got}")).await;
    check_eq!(got, 4, "2 + 2");
    Ok(())
}

async fn math_mul(ctx: &Context) -> Verdict {
    ctx.diagnostic("computing 6 * 9".into()).await;
    let got = 6 * 9;
    ctx.diagnostic(format!("got {got}, expecting the ultimate answer"))
        .await;
    check!(got == 42, "6 * 9: expected 42, got {got}");
    Ok(())
}

async fn token_attest(ctx: &Context) -> Verdict {
    // Demonstrates the runtime escape hatch: a run-stable target fact
    // (a hardware token, per the manifest) turns out not to hold at
    // run time. The case asserts what it can and reports a claim.
    ctx.diagnostic("probing for hardware token".into()).await;
    let token_present = false; // simulated: token unplugged
    if token_present {
        unreachable!("would attest here");
    }
    ctx.diagnostic("token unavailable; asserting clean error".into())
        .await;
    skipped(
        "token unavailable at run time; asserted attestation fails cleanly \
         (no hang, no partial attestation)",
    )
}

fn registry() -> Registry<Context> {
    let mut reg = Registry::new();
    case!(reg, "sample/math/add", [], math_add);
    case!(reg, "sample/math/mul", [], math_mul);
    case!(reg, "sample/token/attest", [], token_attest);
    reg
}

// ------------------------------------------------------ bindings glue

thread_local! {
    static REGISTRY: OnceCell<Registry<Context>> = const { OnceCell::new() };
}

fn with_registry<R>(f: impl FnOnce(&Registry<Context>) -> R) -> R {
    REGISTRY.with(|cell| f(cell.get_or_init(registry)))
}

struct Case {
    index: usize,
}

impl GuestTestCase for Case {
    fn name(&self) -> String {
        with_registry(|reg| reg.get(self.index).unwrap().name.as_str().to_string())
    }

    async fn run(&self, ctx: &Context) -> Result<(), Outcome> {
        let verdict = with_registry(|reg| (reg.get(self.index).unwrap().run)(ctx));
        verdict.await.map_err(|failure| match failure {
            component_test_sdk::Failure::Failed(d) => Outcome::Failed(d),
            component_test_sdk::Failure::Skipped(d) => Outcome::Skipped(d),
        })
    }
}

struct Suite;

impl Guest for Suite {
    type TestCase = Case;

    async fn all() -> Vec<TestCase> {
        with_registry(|reg| (0..reg.len()).map(|index| TestCase::new(Case { index })).collect())
    }
}

bindings::export!(Suite with_types_in bindings);
