//! Host-side formats: the inventory lockfile, the cross-target
//! aggregator (#30) and its markdown matrix renderer, and — re-exported
//! from the guest-linkable `component-test-results` crate — the
//! canonical results model (#26).

pub mod aggregate;
pub mod inventory;
pub mod lockfile;
pub mod manifest;
pub mod matrix;
/// The canonical results model, re-exported: the types live in
/// `component-test-results` so guest-side encoders (e.g. the composed
/// runner core) can link the schema without dragging in wasmparser,
/// TOML, and the aggregator.
pub use component_test_results as results;

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
