//! Fixture suite for runner testing, on the `#[suite]` attribute DX:
//! the trap path, a tagged pair (via tag inheritance + polarity flip),
//! and a generated row.

#[component_test_sdk::suite]
mod fixture {
    mod trap {
        #[case]
        async fn before(ctx: &TestContext) -> Verdict {
            ctx.diagnostic("before the storm".into()).await;
            Ok(())
        }

        #[case]
        async fn boom(ctx: &TestContext) -> Verdict {
            ctx.diagnostic("about to trap".into()).await;
            panic!("fixture trap: deliberate");
        }

        #[case]
        async fn after(ctx: &TestContext) -> Verdict {
            ctx.diagnostic("still alive in a fresh instance".into())
                .await;
            Ok(())
        }
    }

    #[cases(tags("hsm"))]
    mod hsm {
        #[case]
        async fn attest(ctx: &TestContext) -> Verdict {
            ctx.diagnostic("exercising hsm attestation".into()).await;
            Ok(())
        }

        /// Polarity flip: the inherited `hsm` is overridden by `!hsm` —
        /// the decline case lives with its family.
        #[case(tags("!hsm"))]
        async fn declined(ctx: &TestContext) -> Verdict {
            ctx.diagnostic("asserting hsm is declined, not half-served".into())
                .await;
            Ok(())
        }
    }

    /// Generated row: leaves computed from data under a static prefix.
    #[case_generator(prefix = "gen")]
    fn generated_cases() -> impl Iterator<Item = Case<TestContext>> {
        (1u32..=2).map(|n| {
            Case::new(format!("tc{n}"), move |ctx: &TestContext| {
                Box::pin(async move {
                    ctx.diagnostic(format!("generated case {n}")).await;
                    check_eq!(n * 2, n + n, "doubling");
                    Ok(())
                })
            })
        })
    }
}
