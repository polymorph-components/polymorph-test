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

/// The fold selection: lockfile case names plus every case name
/// observed in the stream. Generated-row leaves exist only at run
/// time, so they are knowable solely from the stream — without the
/// union, an all-generated suite trips the "empty selection is a run
/// error" rule spuriously. Reported names are by definition selected;
/// coverage (exact-case completeness, prefix membership, grammar) is
/// enforced separately by `check_coverage`.
pub fn selected_names(lockfile: Option<&lockfile::Lockfile>, stream: &str) -> Vec<String> {
    let mut selected: Vec<String> = lockfile
        .map(|lf| {
            lf.case
                .iter()
                .map(|c| c.name.as_str().to_string())
                .collect()
        })
        .unwrap_or_default();
    let mut seen: std::collections::BTreeSet<String> = selected.iter().cloned().collect();
    for name in stream
        .lines()
        .skip(1) // envelope
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v.get("case").and_then(|c| c.as_str()).map(String::from))
    {
        if seen.insert(name.clone()) {
            selected.push(name);
        }
    }
    selected
}
