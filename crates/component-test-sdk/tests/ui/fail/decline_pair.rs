//! A positively-tagged feature with no `!feature` decline case: the
//! compile-time half of the decline-pair invariant.

#[component_test_sdk::suite]
mod lint {
    #[case(tags("hsm"))]
    async fn attest(_ctx: &TestContext) -> Verdict {
        Ok(())
    }
}

fn main() {}
