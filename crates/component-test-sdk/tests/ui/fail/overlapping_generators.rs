//! Nested generator prefixes: leaves would collide only at run time.

#[component_test_sdk::suite]
mod overlap {
    #[case_generator(prefix = "rows")]
    fn outer() -> impl Iterator<Item = Case> {
        std::iter::empty()
    }

    #[case_generator(prefix = "rows/sub")]
    fn inner() -> impl Iterator<Item = Case> {
        std::iter::empty()
    }
}

fn main() {}
