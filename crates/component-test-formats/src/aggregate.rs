//! Aggregation and validation across targets (#30): join per-target
//! folded [`Document`]s against the lockfile inventory and the target
//! manifest, cross-checking every declaration surface.
//!
//! Validation gates (each refuses a stale reference, which is what
//! makes feature retirement a plain one-commit deletion — see #30):
//! - lockfile + manifest structural validation
//! - closed feature namespace: every lockfile tag names a declared
//!   feature (manifest missing-features entries are checked by
//!   [`Manifest::validate`])
//! - dead declarations: a declared feature no lockfile tag references
//! - unknown targets (results for a target the manifest doesn't
//!   declare) and silent targets (declared but no results; a warning
//!   instead for targets declared `optional = true`)
//! - per-target coverage against the lockfile (every case reported
//!   exactly once; generated leaves under their prefix)
//! - applicability drift: a case's reported status must agree with
//!   what its tags say about the target's missing-features
//! - unterminated segments, run-errors, unknown statuses

use std::collections::{BTreeMap, BTreeSet};

use component_test_core::Tags;

use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::results::{CaseResult, Document, Status};

/// The joined view: per-target case results plus everything validation
/// found. `errors` non-empty means the corpus is invalid regardless of
/// case verdicts.
#[derive(Debug, Clone)]
pub struct Aggregate {
    /// Target names, manifest order.
    pub targets: Vec<String>,
    /// target → case → result, both in canonical (sorted) order.
    pub results: BTreeMap<String, BTreeMap<String, CaseResult>>,
    /// Validation failures.
    pub errors: Vec<String>,
    /// Tolerated oddities (unknown statuses).
    pub warnings: Vec<String>,
}

impl Aggregate {
    /// True when validation passed and no case failed or was left
    /// unreached.
    pub fn ok(&self) -> bool {
        self.errors.is_empty() && !self.has_failures()
    }

    /// Any fail or not-reached status across all targets.
    pub fn has_failures(&self) -> bool {
        self.results
            .values()
            .flat_map(|m| m.values())
            .any(|r| matches!(r.status, Status::Fail | Status::NotReached))
    }
}

/// Join per-target documents against the lockfile and manifest.
pub fn aggregate(
    lockfile: &Lockfile,
    manifest: &Manifest,
    docs: &[(String, Document)],
) -> Aggregate {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let lock_features = match lockfile.validate() {
        Ok(features) => features,
        Err(e) => {
            errors.push(format!("lockfile: {e}"));
            BTreeSet::new()
        }
    };
    if let Err(e) = manifest.validate() {
        errors.push(format!("manifest: {e}"));
    }

    // Closed namespace, both directions: every lockfile tag names a
    // declared feature; every declared feature is referenced by a tag
    // (a dead classification is a retirement leftover).
    for feature in &lock_features {
        if !manifest.features.contains_key(feature) {
            errors.push(format!(
                "lockfile tag names feature `{feature}`, which is not classified \
                 in the manifest [features] table"
            ));
        }
    }
    // Every case must be applicable on at least one declared target:
    // otherwise it is dead coverage (e.g. a decline case whose feature
    // no target lacks) that no run will ever exercise.
    {
        let target_missing: Vec<&[String]> = manifest
            .targets
            .values()
            .map(|t| t.missing_features.as_slice())
            .collect();
        let applicable_somewhere =
            |tags: &component_test_core::Tags| target_missing.iter().any(|m| tags.applies(m));
        for entry in &lockfile.case {
            let tags =
                component_test_core::Tags::new(entry.tags.clone()).expect("lockfile validated");
            if !applicable_somewhere(&tags) {
                errors.push(format!(
                    "case `{}` is applicable on no declared target (dead coverage)",
                    entry.name
                ));
            }
        }
        for gen in &lockfile.generated {
            let tags =
                component_test_core::Tags::new(gen.tags.clone()).expect("lockfile validated");
            if !applicable_somewhere(&tags) {
                errors.push(format!(
                    "generated row `{}/*` is applicable on no declared target (dead coverage)",
                    gen.prefix
                ));
            }
        }
    }

    for (feature, decl) in &manifest.features {
        // Structural features gate suites at composition time and never
        // appear as case tags (README: "structural features gate
        // suites, not cases") — only gated features must be referenced.
        if decl.kind == crate::manifest::FeatureKind::Structural {
            continue;
        }
        if !lock_features.contains(feature) {
            errors.push(format!(
                "manifest classifies gated feature `{feature}`, which no lockfile tag \
                 references (retired feature not fully deleted?)"
            ));
        }
    }

    let mut results: BTreeMap<String, BTreeMap<String, CaseResult>> = BTreeMap::new();
    let mut seen_targets = BTreeSet::new();

    for (target, doc) in docs {
        if !seen_targets.insert(target.as_str()) {
            errors.push(format!("target `{target}`: multiple result sets"));
            continue;
        }
        if doc.envelope.target != *target {
            warnings.push(format!(
                "target `{target}`: results envelope names target `{}`",
                doc.envelope.target
            ));
        }
        let missing = match manifest.missing(target) {
            Some(missing) => missing,
            None => {
                errors.push(format!(
                    "target `{target}`: not declared in the manifest [targets] table"
                ));
                &[]
            }
        };

        if !doc.terminated {
            errors.push(format!("target `{target}`: unterminated segment"));
        }
        for e in &doc.run_errors {
            errors.push(format!("target `{target}`: run error: {e}"));
        }
        for (case, status) in &doc.unknown_statuses {
            warnings.push(format!(
                "target `{target}`: case `{case}` has unknown status `{status}`"
            ));
        }

        if let Err(e) = lockfile.check_coverage(doc.results.iter().map(|r| r.case.as_str())) {
            errors.push(format!("target `{target}`: coverage: {e}"));
        }

        // Applicability drift: the reported status must agree with the
        // case's tags applied to this target's missing-features.
        let case_tags: BTreeMap<std::borrow::Cow<'_, str>, &Vec<component_test_core::Tag>> =
            lockfile
                .case
                .iter()
                .map(|c| (c.name.as_str(), &c.tags))
                .collect();
        for r in &doc.results {
            let tags = case_tags
                .get(r.case.as_str())
                .copied()
                .or_else(|| lockfile.prefix_of(&r.case).map(|g| &g.tags));
            let Some(tags) = tags else { continue }; // coverage already flagged
            let Ok(tags) = Tags::new(tags.clone()) else {
                continue;
            };
            let applies = tags.applies(missing);
            match (applies, r.status) {
                (false, Status::NotApplicable) | (false, Status::NotReached) => {}
                (false, _) => errors.push(format!(
                    "target `{target}`: case `{}` reported {:?} but its tags make it \
                     not applicable given missing-features {missing:?}",
                    r.case, r.status
                )),
                (true, Status::NotApplicable) => errors.push(format!(
                    "target `{target}`: case `{}` reported not-applicable but its tags \
                     apply given missing-features {missing:?}",
                    r.case
                )),
                (true, _) => {}
            }
        }

        results.insert(
            target.clone(),
            doc.results
                .iter()
                .map(|r| (r.case.clone(), r.clone()))
                .collect(),
        );
    }

    for (target, decl) in &manifest.targets {
        if !seen_targets.contains(target.as_str()) {
            if decl.optional {
                warnings.push(format!("target `{target}`: no results (declared optional)"));
            } else {
                errors.push(format!("target `{target}`: no results"));
            }
        }
    }

    Aggregate {
        targets: manifest.targets.keys().cloned().collect(),
        results,
        errors,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::{CaseEntry, SuiteRef};
    use crate::manifest::Manifest;
    use crate::results::{Envelope, RunInfo, SuiteInfo, RESULTS_VERSION};
    use component_test_core::{CaseName, Provenance, Tag};

    fn entry(name: &str, tags: &[&str]) -> CaseEntry {
        CaseEntry {
            name: CaseName::parse(name).unwrap(),
            tags: tags.iter().map(|t| Tag::parse(t).unwrap()).collect(),
        }
    }

    fn lockfile(cases: Vec<CaseEntry>) -> Lockfile {
        Lockfile::new(
            SuiteRef {
                name: "suite".into(),
                artifact_sha256: None,
            },
            cases,
        )
    }

    fn result(case: &str, status: Status, detail: Option<&str>) -> CaseResult {
        CaseResult {
            case: case.into(),
            status,
            provenance: status.executed().then_some(Provenance::Returned),
            detail: detail.map(String::from),
            seed: None,
            duration_ms: None,
            diagnostics: vec![],
            diagnostics_complete: true,
        }
    }

    fn doc(target: &str, results: Vec<CaseResult>) -> Document {
        Document {
            envelope: Envelope {
                version: RESULTS_VERSION.into(),
                target: target.into(),
                suite: SuiteInfo {
                    name: "suite".into(),
                    ..Default::default()
                },
                run: RunInfo::default(),
            },
            results,
            run_errors: vec![],
            unknown_statuses: BTreeMap::new(),
            terminated: true,
        }
    }

    fn manifest(toml: &str) -> Manifest {
        Manifest::from_toml(toml).unwrap()
    }

    const MANIFEST: &str = r#"
        version = "0.1"
        [features.hsm]
        kind = "gated"
        [targets.native]
        missing-features = []
        [targets.sim]
        missing-features = ["hsm"]
    "#;

    fn corpus_lock() -> Lockfile {
        lockfile(vec![
            entry("suite/add", &[]),
            entry("suite/hsm/attest", &["hsm"]),
            entry("suite/hsm/declined", &["!hsm"]),
        ])
    }

    fn native_doc() -> Document {
        doc(
            "native",
            vec![
                result("suite/add", Status::Pass, None),
                result("suite/hsm/attest", Status::Pass, None),
                result("suite/hsm/declined", Status::NotApplicable, Some("!hsm")),
            ],
        )
    }

    fn sim_doc() -> Document {
        doc(
            "sim",
            vec![
                result("suite/add", Status::Pass, None),
                result("suite/hsm/attest", Status::NotApplicable, Some("hsm")),
                result("suite/hsm/declined", Status::Pass, None),
            ],
        )
    }

    #[test]
    fn clean_corpus_validates() {
        let agg = aggregate(
            &corpus_lock(),
            &manifest(MANIFEST),
            &[("native".into(), native_doc()), ("sim".into(), sim_doc())],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);
        assert!(agg.ok());
        assert_eq!(agg.targets, ["native", "sim"]);
    }

    #[test]
    fn unknown_target_is_error() {
        let agg = aggregate(
            &corpus_lock(),
            &manifest(MANIFEST),
            &[
                ("native".into(), native_doc()),
                ("sim".into(), sim_doc()),
                ("mystery".into(), doc("mystery", vec![])),
            ],
        );
        assert!(
            agg.errors
                .iter()
                .any(|e| e.contains("mystery") && e.contains("not declared")),
            "{:?}",
            agg.errors
        );
    }

    #[test]
    fn silent_target_is_error() {
        let agg = aggregate(
            &corpus_lock(),
            &manifest(MANIFEST),
            &[("native".into(), native_doc())],
        );
        assert!(
            agg.errors
                .iter()
                .any(|e| e.contains("`sim`") && e.contains("no results")),
            "{:?}",
            agg.errors
        );
    }

    #[test]
    fn silent_optional_target_is_warning() {
        let optional = manifest(
            r#"
            version = "0.1"
            [features.hsm]
            kind = "gated"
            [targets.native]
            missing-features = []
            [targets.sim]
            missing-features = ["hsm"]
            optional = true
        "#,
        );
        let agg = aggregate(
            &corpus_lock(),
            &optional,
            &[("native".into(), native_doc())],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);
        assert!(
            agg.warnings
                .iter()
                .any(|w| w.contains("`sim`") && w.contains("no results")),
            "{:?}",
            agg.warnings
        );
        // When the optional target does report, it is validated like any
        // other target: an applicability drift is still an error.
        let mut drifted = sim_doc();
        drifted.results[1] = result("suite/hsm/attest", Status::Pass, None);
        let agg = aggregate(
            &corpus_lock(),
            &optional,
            &[("native".into(), native_doc()), ("sim".into(), drifted)],
        );
        assert!(
            agg.errors.iter().any(|e| e.contains("not applicable")),
            "{:?}",
            agg.errors
        );
    }

    #[test]
    fn coverage_mismatch_is_error() {
        let mut d = native_doc();
        d.results.pop();
        let agg = aggregate(
            &corpus_lock(),
            &manifest(MANIFEST),
            &[("native".into(), d), ("sim".into(), sim_doc())],
        );
        assert!(
            agg.errors.iter().any(|e| e.contains("coverage")),
            "{:?}",
            agg.errors
        );
    }

    #[test]
    fn unterminated_and_run_errors_are_errors() {
        let mut d = native_doc();
        d.terminated = false;
        d.run_errors.push("enumeration trapped".into());
        let agg = aggregate(
            &corpus_lock(),
            &manifest(MANIFEST),
            &[("native".into(), d), ("sim".into(), sim_doc())],
        );
        assert!(agg.errors.iter().any(|e| e.contains("unterminated")));
        assert!(agg.errors.iter().any(|e| e.contains("enumeration trapped")));
    }

    #[test]
    fn applicability_drift_is_error() {
        // sim declares hsm missing, but reports the hsm-tagged case as
        // executed (adapter/manifest drift).
        let mut d = sim_doc();
        d.results[1] = result("suite/hsm/attest", Status::Pass, None);
        let agg = aggregate(
            &corpus_lock(),
            &manifest(MANIFEST),
            &[("native".into(), native_doc()), ("sim".into(), d)],
        );
        assert!(
            agg.errors.iter().any(|e| e.contains("not applicable")),
            "{:?}",
            agg.errors
        );
    }

    #[test]
    fn undeclared_lockfile_feature_is_error() {
        let manifest = manifest(
            r#"
            version = "0.1"
            [targets.native]
            missing-features = []
            [targets.sim]
            missing-features = []
        "#,
        );
        let agg = aggregate(
            &corpus_lock(),
            &manifest,
            &[("native".into(), native_doc()), ("sim".into(), sim_doc())],
        );
        assert!(
            agg.errors
                .iter()
                .any(|e| e.contains("`hsm`") && e.contains("not classified")),
            "{:?}",
            agg.errors
        );
    }

    /// The #30 retirement acceptance test: a corpus with a tagged pair
    /// + manifest entry. Every partial-deletion state must fail
    ///   validation; the complete deletion must pass.
    #[test]
    fn feature_retirement() {
        // Retirement removes, in one commit: (A) the tagged cases from
        // the suite/lockfile, (B) the [features] classification, (C)
        // the missing-features entries. Results follow the suite.
        let retired_lock = lockfile(vec![entry("suite/add", &[])]);
        let retired_manifest = manifest(
            r#"
            version = "0.1"
            [targets.native]
            missing-features = []
            [targets.sim]
            missing-features = []
        "#,
        );
        let retired_docs = |t: &str| doc(t, vec![result("suite/add", Status::Pass, None)]);
        let stale_docs = |t: &str| {
            if t == "native" {
                native_doc()
            } else {
                sim_doc()
            }
        };

        // Enumerate the 2^3 - 2 partial states: for each surface, keep
        // the stale version or the retired one.
        for (del_a, del_b, del_c) in itertools() {
            let lock = if del_a {
                &retired_lock
            } else {
                &corpus_lock2()
            };
            let mani = if del_b && del_c {
                retired_manifest.clone()
            } else {
                partial_manifest(del_b, del_c)
            };
            let docs: Vec<(String, Document)> = ["native", "sim"]
                .iter()
                .map(|t| {
                    (
                        t.to_string(),
                        if del_a {
                            retired_docs(t)
                        } else {
                            stale_docs(t)
                        },
                    )
                })
                .collect();
            let agg = aggregate(lock, &mani, &docs);
            let uniform = del_a == del_b && del_b == del_c;
            if uniform {
                // All-false is the valid pre-retirement baseline;
                // all-true is the complete deletion. Both pass.
                assert!(
                    agg.errors.is_empty(),
                    "uniform state (A={del_a} B={del_b} C={del_c}): {:?}",
                    agg.errors
                );
            } else {
                assert!(
                    !agg.errors.is_empty(),
                    "partial deletion (A={del_a} B={del_b} C={del_c}) passed validation"
                );
            }
        }

        fn itertools() -> Vec<(bool, bool, bool)> {
            (0..8u8)
                .map(|i| (i & 1 != 0, i & 2 != 0, i & 4 != 0))
                .collect()
        }
        fn corpus_lock2() -> Lockfile {
            lockfile(vec![
                entry("suite/add", &[]),
                entry("suite/hsm/attest", &["hsm"]),
                entry("suite/hsm/declined", &["!hsm"]),
            ])
        }
        fn partial_manifest(del_b: bool, del_c: bool) -> Manifest {
            let features = if del_b {
                ""
            } else {
                "[features.hsm]\nkind = \"gated\"\n"
            };
            let sim_missing = if del_c { "[]" } else { "[\"hsm\"]" };
            Manifest::from_toml(&format!(
                "version = \"0.1\"\n{features}\
                 [targets.native]\nmissing-features = []\n\
                 [targets.sim]\nmissing-features = {sim_missing}\n"
            ))
            .unwrap()
        }
    }
}

#[cfg(test)]
mod dead_coverage_tests {
    use super::*;
    use crate::lockfile::{CaseEntry, Lockfile, SuiteRef};
    use crate::manifest::Manifest;
    use component_test_core::{CaseName, Tag};

    #[test]
    fn case_applicable_nowhere_is_error() {
        let lock = Lockfile::new(
            SuiteRef {
                name: "s".into(),
                artifact_sha256: None,
            },
            vec![
                CaseEntry {
                    name: CaseName::parse("a/pos").unwrap(),
                    tags: vec![Tag::parse("hsm").unwrap()],
                },
                CaseEntry {
                    name: CaseName::parse("a/neg").unwrap(),
                    tags: vec![Tag::parse("!hsm").unwrap()],
                },
            ],
        );
        // Only a full-support target: the decline case runs nowhere.
        let manifest: Manifest = toml::from_str(
            r#"
            version = "0.1"
            [features.hsm]
            kind = "gated"
            [targets.full]
            missing-features = []
            "#,
        )
        .unwrap();
        let agg = aggregate(&lock, &manifest, Default::default());
        assert!(
            agg.errors
                .iter()
                .any(|e| e.contains("a/neg") && e.contains("no declared target")),
            "{:?}",
            agg.errors
        );
        // Adding a target lacking hsm clears it.
        let manifest: Manifest = toml::from_str(
            r#"
            version = "0.1"
            [features.hsm]
            kind = "gated"
            [targets.full]
            missing-features = []
            [targets.lacking]
            missing-features = ["hsm"]
            "#,
        )
        .unwrap();
        let agg = aggregate(&lock, &manifest, Default::default());
        assert!(
            !agg.errors.iter().any(|e| e.contains("no declared target")),
            "{:?}",
            agg.errors
        );
    }
}
