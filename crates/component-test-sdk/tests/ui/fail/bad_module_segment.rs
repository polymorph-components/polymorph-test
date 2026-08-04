//! Non-leaf segments must be WIT labels; the error must point at the
//! offending module, not at a case fn two lines down.

#[component_test_sdk::suite]
mod suite {
    mod x__y {
        #[case]
        async fn go(_ctx: &TestContext) -> Verdict {
            Ok(())
        }
    }
}

fn main() {}
