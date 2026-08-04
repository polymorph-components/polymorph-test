//! `#[suite(name = "")]`: the unrooted mode for ported corpora whose
//! established case ids share no common root segment.

#[component_test_sdk::suite(name = "")]
mod ported {
    mod aes_gcm {
        #[case]
        async fn tc1(_ctx: &TestContext) -> Verdict {
            Ok(())
        }
    }

    mod chacha {
        #[case]
        async fn tc1(_ctx: &TestContext) -> Verdict {
            Ok(())
        }
    }
}

fn main() {}
