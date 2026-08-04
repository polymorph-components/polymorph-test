//! Host-side formats: the inventory lockfile and the canonical results
//! model (#26): WIT-shaped event records, JSONL edge encoding, and the
//! stream→document fold (including the `not-reached` rule). Plus the
//! cross-target aggregator (#30) and its markdown matrix renderer.

pub mod aggregate;
pub mod inventory;
pub mod lockfile;
pub mod manifest;
pub mod matrix;
pub mod results;

/// Hex-encoded sha256: the artifact-binding digest recorded in
/// lockfiles (`suite.artifact_sha256`) and results envelopes
/// (`artifact-sha256`), cross-checked by [`aggregate::aggregate`].
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
