//! Two cases folding to the same derived name.

#[component_test_sdk::suite]
mod dupe {
    #[case]
    async fn add(_ctx: &TestContext) -> Verdict {
        Ok(())
    }

    #[case(name = "add")]
    async fn add_again(_ctx: &TestContext) -> Verdict {
        Ok(())
    }
}

fn main() {}
