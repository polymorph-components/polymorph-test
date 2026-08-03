//! Fixture suite for runner testing: exercises the trap path (a case
//! that traps mid-run, after emitting a diagnostic) and feature-mark
//! scheduling (a marked pair). Case order is chosen so the trap has
//! cases after it: continuation proves per-case isolation.

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
use component_test_sdk::{case, Registry, Verdict};

async fn before(ctx: &Context) -> Verdict {
    ctx.diagnostic("before the storm".into()).await;
    Ok(())
}

async fn boom(ctx: &Context) -> Verdict {
    ctx.diagnostic("about to trap".into()).await;
    panic!("fixture trap: deliberate");
}

async fn after(ctx: &Context) -> Verdict {
    ctx.diagnostic("still alive in a fresh instance".into())
        .await;
    Ok(())
}

async fn hsm_attest(ctx: &Context) -> Verdict {
    ctx.diagnostic("exercising hsm attestation".into()).await;
    Ok(())
}

async fn hsm_declined(ctx: &Context) -> Verdict {
    ctx.diagnostic("asserting hsm is declined, not half-served".into())
        .await;
    Ok(())
}

fn registry() -> Registry<Context> {
    let mut reg = Registry::new();
    case!(reg, "fixture/trap/before", [], before);
    case!(reg, "fixture/trap/boom", [], boom);
    case!(reg, "fixture/trap/after", [], after);
    case!(reg, "fixture/hsm/attest", ["hsm"], hsm_attest);
    case!(reg, "fixture/hsm/declined", ["!hsm"], hsm_declined);
    reg
}

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
        with_registry(|reg| {
            (0..reg.len())
                .map(|index| TestCase::new(Case { index }))
                .collect()
        })
    }
}

bindings::export!(Suite with_types_in bindings);
