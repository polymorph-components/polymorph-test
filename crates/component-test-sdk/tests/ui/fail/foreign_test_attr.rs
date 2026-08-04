//! S1: a foreign test attribute inside a suite would be a silently
//! dropped case (classic porting mistake).

#[component_test_sdk::suite]
mod ported {
    #[case]
    async fn real(_ctx: &TestContext) -> Verdict {
        Ok(())
    }

    #[test]
    fn forgotten_rename() {}
}

fn main() {}
