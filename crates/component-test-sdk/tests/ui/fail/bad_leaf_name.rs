//! Leaf-name override still goes through the case-name grammar
//! (`..` is forbidden: results keys must be filesystem-safe).

#[component_test_sdk::suite]
mod named {
    #[case(name = "..")]
    async fn traversal(_ctx: &TestContext) -> Verdict {
        Ok(())
    }
}

fn main() {}
