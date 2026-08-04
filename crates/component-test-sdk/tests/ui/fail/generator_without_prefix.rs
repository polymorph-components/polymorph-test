//! Generators must declare their static prefix (it is the inventory
//! record).

#[component_test_sdk::suite]
mod nameless {
    #[case_generator()]
    fn rows() -> impl Iterator<Item = Case> {
        std::iter::empty()
    }
}

fn main() {}
