//! Per-target capability manifest (#30): the closed feature namespace
//! (a central `[features]` classification table) plus the per-target
//! missing-features declarations. TOML, hand-maintained, additive:
//! unknown keys are tolerated.
//!
//! The aggregator validates every declaration surface against the
//! `[features]` table — target missing-features entries here, and
//! lockfile feature tags (see [`crate::aggregate`]). Unknown names are
//! run-failing errors: this is the typo guard that replaced
//! trap-on-unknown, and the gate that makes feature retirement safe
//! (a stale reference anywhere fails validation).

use std::collections::BTreeMap;

use anyhow::{bail, Context as _};
use serde::{Deserialize, Serialize};

pub const MANIFEST_VERSION: &str = "0.1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// Format version.
    pub version: String,
    /// The closed feature namespace: every declarable feature name and
    /// its classification.
    #[serde(default)]
    pub features: BTreeMap<String, Feature>,
    /// Targets under test, keyed by opaque target name
    /// (implementation × environment).
    #[serde(default)]
    pub targets: BTreeMap<String, Target>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Feature {
    pub kind: FeatureKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FeatureKind {
    /// `@unstable`-gated WIT surface: the guest imports it, so a target
    /// declaring it missing must still answer — decline-pair probes
    /// police the declaration at runtime.
    Gated,
    /// Not imported at all: nothing to observe at runtime; policed by
    /// composition gates, not probes.
    Structural,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Target {
    /// The features this target is missing. The negative baseline:
    /// unlisted features are served.
    #[serde(default, rename = "missing-features")]
    pub missing_features: Vec<String>,
    /// An optional target's absent results are a warning, not an error:
    /// for targets whose environment is not always available (e.g. a
    /// browser gate that runs in CI and behind a local opt-in). Present
    /// results are validated exactly like any other target's.
    #[serde(default)]
    pub optional: bool,
}

impl Manifest {
    pub fn from_toml(s: &str) -> anyhow::Result<Self> {
        let m: Manifest = toml::from_str(s).context("parsing manifest")?;
        Ok(m)
    }

    /// Structural validation: version, non-empty targets table, and
    /// every missing-features entry classified in `[features]`.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.version != MANIFEST_VERSION {
            bail!("unsupported manifest version {}", self.version);
        }
        if self.targets.is_empty() {
            bail!("manifest declares no targets");
        }
        for (name, target) in &self.targets {
            for feature in &target.missing_features {
                if !self.features.contains_key(feature) {
                    bail!(
                        "target `{name}`: missing-features entry `{feature}` is not \
                         classified in [features] — only declared features may be \
                         declared missing"
                    );
                }
            }
        }
        Ok(())
    }

    /// The missing-features list for a target, if declared.
    pub fn missing(&self, target: &str) -> Option<&[String]> {
        self.targets
            .get(target)
            .map(|t| t.missing_features.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
        version = "0.1"
        [features.chacha20-poly1305]
        kind = "gated"
        [features.ecdsa-sign]
        kind = "structural"
        [targets.wasmtime-rustcrypto]
        missing-features = []
        [targets.jco-node]
        missing-features = ["chacha20-poly1305"]
    "#;

    #[test]
    fn parse_and_validate() {
        let m = Manifest::from_toml(GOOD).unwrap();
        m.validate().unwrap();
        assert_eq!(m.features["ecdsa-sign"].kind, FeatureKind::Structural);
        assert_eq!(
            m.missing("jco-node").unwrap(),
            ["chacha20-poly1305".to_string()]
        );
        assert!(m.missing("nonexistent").is_none());
        assert!(!m.targets["jco-node"].optional);
    }

    #[test]
    fn optional_target_parses() {
        let m = Manifest::from_toml(
            r#"
            version = "0.1"
            [targets.browser]
            missing-features = []
            optional = true
        "#,
        )
        .unwrap();
        m.validate().unwrap();
        assert!(m.targets["browser"].optional);
    }

    #[test]
    fn unclassified_missing_feature_refused() {
        let m = Manifest::from_toml(
            r#"
            version = "0.1"
            [targets.a]
            missing-features = ["some-quirk"]
        "#,
        )
        .unwrap();
        let err = m.validate().unwrap_err().to_string();
        assert!(err.contains("some-quirk"), "{err}");
    }

    #[test]
    fn empty_targets_refused() {
        let m = Manifest::from_toml(r#"version = "0.1""#).unwrap();
        let err = m.validate().unwrap_err().to_string();
        assert!(err.contains("no targets"), "{err}");
    }

    #[test]
    fn unknown_keys_tolerated() {
        let m = Manifest::from_toml(
            r#"
            version = "0.1"
            future-top-level = "x"
            [features.f]
            kind = "gated"
            note = "why"
            [targets.a]
            missing-features = ["f"]
            optional = true
        "#,
        )
        .unwrap();
        m.validate().unwrap();
    }
}
