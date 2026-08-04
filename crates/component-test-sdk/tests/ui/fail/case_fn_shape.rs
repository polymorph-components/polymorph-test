//! M4: case fns take no arguments or exactly `(ctx: &TestContext)`.

#[component_test_sdk::suite]
mod shape {
    #[case]
    async fn two_args(_ctx: &TestContext, _extra: u32) -> Verdict {
        Ok(())
    }
}

fn main() {}
