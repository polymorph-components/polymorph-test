//! The `#[suite]` rustdoc example, kept compiling: nested modules,
//! tag inheritance with a polarity flip, a generator row, and the
//! full native expansion (export glue + tags link-section). If this
//! breaks, the doc example on `suite` is broken too — update both.

#[component_test_sdk::suite]
mod sample {
    mod math {
        #[case]
        async fn add(ctx: &TestContext) -> Verdict {
            ctx.diag("computing").await;
            check_eq!(2 + 2, 4, "2 + 2");
            Ok(())
        }

        #[case]
        fn quick() -> Verdict {
            Ok(())
        }
    }

    #[cases(tags("hsm"))]
    mod hsm {
        #[case]
        async fn attest(_ctx: &TestContext) -> Verdict {
            Ok(())
        }

        #[case(tags("!hsm"))]
        async fn declined(_ctx: &TestContext) -> Verdict {
            Ok(())
        }
    }

    #[case_generator(prefix = "gen")]
    fn cases() -> impl Iterator<Item = Case> {
        (1..=2).map(|n| {
            gen_case!(format!("tc{n}"), |ctx| async move {
                ctx.diag(format!("case {n}")).await;
                Ok(())
            })
        })
    }
}

fn main() {}
