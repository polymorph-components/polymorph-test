//! Verdicts: what an executed case yields, plus runner-side provenance.

use core::fmt;

/// The verdict of an executed case, mirroring the WIT
/// `result<_, outcome>`: `Ok(()) = pass`.
pub type Verdict = Result<(), Failure>;

/// The WIT `outcome` variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(tag = "outcome", content = "detail", rename_all = "kebab-case")
)]
pub enum Failure {
    /// Observed behavior diverged; the payload says how, in one line.
    Failed(String),
    /// The case ran but could not reach its subject; the payload says
    /// what it asserted instead. Exceptional — gating knowable before
    /// the run belongs in feature tags.
    Skipped(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Failed(d) => write!(f, "failed: {d}"),
            Failure::Skipped(d) => write!(f, "skipped: {d}"),
        }
    }
}

/// How a `fail` result came to be (results-schema `provenance`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "kebab-case")
)]
pub enum Provenance {
    /// The case returned the verdict itself.
    Returned,
    /// A wasm trap attributed to this case; the instance is poisoned.
    Trap,
    /// The runner's hang guard tripped; the instance was abandoned.
    HangGuard,
}
