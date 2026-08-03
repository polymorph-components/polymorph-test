//! Core data model for `lann:component-test`: the normative case-name
//! grammar, feature tags, and verdicts. Shared by guest SDKs and host
//! tooling; deliberately dependency-light.

pub mod name;
pub mod tags;

pub use arcstr::{self, ArcStr};
pub mod verdict;

pub use name::{normalize_segment, CaseName, NameError};
pub use tags::{Tag, TagParseError, Tags, TagsError};
pub use verdict::{Failure, Provenance, Verdict};
