//! Fixture suite for runner testing, on the `#[suite]` attribute DX:
//! the trap path, a tagged pair (via tag inheritance + polarity flip),
//! and a generated row.

#[component_test_sdk::suite]
mod fixture {
    mod trap {
        #[case]
        async fn before(ctx: &TestContext) -> Verdict {
            ctx.diag("before the storm").await;
            Ok(())
        }

        #[case]
        async fn boom(ctx: &TestContext) -> Verdict {
            ctx.diag("about to trap").await;
            panic!("fixture trap: deliberate");
        }

        #[case]
        async fn after(ctx: &TestContext) -> Verdict {
            ctx.diag("still alive in a fresh instance").await;
            Ok(())
        }
    }

    #[cases(tags("hsm"))]
    mod hsm {
        #[case]
        async fn attest(ctx: &TestContext) -> Verdict {
            ctx.diag("exercising hsm attestation").await;
            Ok(())
        }

        /// Polarity flip: the inherited `hsm` is overridden by `!hsm` —
        /// the decline case lives with its family.
        #[case(tags("!hsm"))]
        async fn declined(ctx: &TestContext) -> Verdict {
            ctx.diag("asserting hsm is declined, not half-served").await;
            Ok(())
        }
    }

    mod nested {
        /// Depth-2 regression: module publicity is macro-managed.
        mod deep {
            #[case]
            async fn leaf(ctx: &TestContext) -> Verdict {
                ctx.diag("two levels down").await;
                Ok(())
            }
        }
    }

    /// Generated row: leaves computed from data under a static prefix.
    #[case_generator(prefix = "gen")]
    fn generated_cases() -> impl Iterator<Item = Case> {
        (1u32..=2).map(|n| {
            gen_case!(format!("tc{n}"), |ctx| async move {
                ctx.diag(format!("generated case {n}")).await;
                check_eq!(n * 2, n + n, "doubling");
                Ok(())
            })
        })
    }
}
