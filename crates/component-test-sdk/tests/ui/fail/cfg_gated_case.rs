//! S3: the inventory must not depend on build configuration.

#[component_test_sdk::suite]
mod gated {
    #[case]
    #[cfg(target_os = "linux")]
    async fn sometimes(_ctx: &TestContext) -> Verdict {
        Ok(())
    }

    #[case]
    async fn always(_ctx: &TestContext) -> Verdict {
        Ok(())
    }
}

fn main() {}
