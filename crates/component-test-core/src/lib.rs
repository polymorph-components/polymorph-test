//! Core data model for `lann:component-test`: the normative case-name
//! grammar, feature marks, and verdicts. Shared by guest SDKs and host
//! tooling; deliberately dependency-light.

pub mod marks;
pub mod name;
pub mod verdict;

pub use marks::{Mark, MarkParseError, Marks, MarksError};
pub use name::{normalize_segment, CaseName, NameError};
pub use verdict::{Failure, Provenance, Verdict};
