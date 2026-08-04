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

/// `?`-operator support: any real error becomes a one-line `failed`.
/// (Blanket over `Error`, not `Display`: `Failure` is itself `Display`,
/// which would collide with the reflexive `From`. No `From<String>`
/// either — coherence forbids it alongside the blanket; use
/// [`failed`](https://docs.rs/component-test-sdk) for string
/// early-exits.) Skips are never produced via `?` — only explicitly.
impl<E: std::error::Error> From<E> for Failure {
    fn from(e: E) -> Self {
        Failure::Failed(e.to_string())
    }
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
///
/// `#[non_exhaustive]`: the results schema evolves additively (frozen
/// surface #3), so downstream matches must carry a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
