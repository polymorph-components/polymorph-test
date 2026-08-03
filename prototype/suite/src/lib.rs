//! Sample test suite: a passing case, a failing case, and a runtime-
//! skipped case (the exceptional escape hatch), all emitting diagnostics.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "suite",
        generate_all,
    });
}

use bindings::exports::lann::component_test::tests::{Guest, GuestTestCase, Outcome, TestCase};
use bindings::lann::component_test::test_context::Context;

#[derive(Clone, Copy)]
enum Kind {
    AddPasses,
    MulFails,
    TokenSkips,
}

struct Case {
    kind: Kind,
}

impl Case {
    fn all() -> Vec<Case> {
        [Kind::AddPasses, Kind::MulFails, Kind::TokenSkips]
            .into_iter()
            .map(|kind| Case { kind })
            .collect()
    }
}

impl GuestTestCase for Case {
    fn name(&self) -> String {
        match self.kind {
            Kind::AddPasses => "sample/math/add",
            Kind::MulFails => "sample/math/mul",
            Kind::TokenSkips => "sample/token/attest",
        }
        .into()
    }

    async fn run(&self, ctx: &Context) -> Result<(), Outcome> {
        match self.kind {
            Kind::AddPasses => {
                ctx.diagnostic("computing 2 + 2".into()).await;
                let got = 2 + 2;
                ctx.diagnostic(format!("got {got}")).await;
                if got == 4 {
                    Ok(())
                } else {
                    Err(Outcome::Failed(format!("2 + 2: expected 4, got {got}")))
                }
            }
            Kind::MulFails => {
                ctx.diagnostic("computing 6 * 9".into()).await;
                let got = 6 * 9;
                ctx.diagnostic(format!("got {got}, expecting the ultimate answer"))
                    .await;
                if got == 42 {
                    Ok(())
                } else {
                    Err(Outcome::Failed(format!("6 * 9: expected 42, got {got}")))
                }
            }
            Kind::TokenSkips => {
                // Demonstrates the runtime escape hatch: a run-stable
                // target fact (a hardware token, per the manifest) turns
                // out not to hold at run time. The case asserts what it
                // can and reports a claim.
                ctx.diagnostic("probing for hardware token".into()).await;
                let token_present = false; // simulated: token unplugged
                if token_present {
                    unreachable!("would attest here");
                } else {
                    ctx.diagnostic("token unavailable; asserting clean error".into())
                        .await;
                    Err(Outcome::Skipped(
                        "token unavailable at run time; asserted attestation \
                         fails cleanly (no hang, no partial attestation)"
                            .into(),
                    ))
                }
            }
        }
    }
}

struct Suite;

impl Guest for Suite {
    type TestCase = Case;

    fn all() -> Vec<TestCase> {
        Case::all().into_iter().map(TestCase::new).collect()
    }
}

bindings::export!(Suite with_types_in bindings);
