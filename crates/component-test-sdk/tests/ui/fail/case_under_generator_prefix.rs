//! An exact case under a generator's prefix would collide with
//! generated rows only at run time; rejected statically.

#[component_test_sdk::suite]
mod overlap {
    mod gen {
        #[case]
        async fn tc1(_ctx: &TestContext) -> Verdict {
            Ok(())
        }
    }

    #[case_generator(prefix = "gen")]
    fn rows() -> impl Iterator<Item = Case> {
        std::iter::empty()
    }
}

fn main() {}
