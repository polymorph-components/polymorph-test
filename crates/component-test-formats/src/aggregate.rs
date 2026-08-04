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
            .any(|r| r.status.failing())
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

    // Applicability lookup, built once (not per target — the corpus
    // scale is thousands of cases × several targets): case name →
    // validated Tags, plus per-prefix Tags for generated rows. Entries
    // whose tag sets don't validate are skipped exactly as the old
    // per-result construction skipped them (lockfile.validate already
    // reported those).
    let case_tags: BTreeMap<std::borrow::Cow<'_, str>, Tags> = lockfile
        .case
        .iter()
        .filter_map(|c| Tags::new(c.tags.clone()).ok().map(|t| (c.name.as_str(), t)))
        .collect();
    let gen_tags: BTreeMap<&str, Tags> = lockfile
        .generated
        .iter()
        .filter_map(|g| {
            Tags::new(g.tags.clone())
                .ok()
                .map(|t| (g.prefix.as_str(), t))
        })
        .collect();
    let tags_for = |case: &str| -> Option<&Tags> {
        case_tags.get(case).or_else(|| {
            lockfile
                .prefix_of(case)
                .and_then(|g| gen_tags.get(g.prefix.as_str()))
        })
    };

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
        // Artifact binding: results produced from a different suite
        // build than the lockfile describes are exactly the drift the
        // sha256 exists to refuse (same names, different bodies).
        // Envelopes without a hash (foreign runners, older streams)
        // are tolerated.
        if let (Some(reported), Some(expected)) = (
            doc.envelope.suite.artifact_sha256.as_deref(),
            lockfile.suite.artifact_sha256.as_deref(),
        ) {
            if reported != expected {
                errors.push(format!(
                    "target `{target}`: results were produced from suite artifact \
                     sha256 {reported}, but the lockfile records {expected} \
                     (stale lockfile or wrong suite build — regenerate with \
                     `component-test lock`)"
                ));
            }
        }
        if doc.envelope.suite.name != lockfile.suite.name {
            warnings.push(format!(
                "target `{target}`: results envelope names suite `{}`, lockfile \
                 names `{}`",
                doc.envelope.suite.name, lockfile.suite.name
            ));
        }
        let missing: Option<&[String]> = match manifest.missing(target) {
            Some(missing) => Some(missing),
            None => {
                errors.push(format!(
                    "target `{target}`: not declared in the manifest [targets] table"
                ));
                // No declared missing-set to judge against: skip the
                // applicability checks below rather than fabricating
                // one and cascading a false drift error per tagged
                // case on top of the root cause.
                None
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

        let mut target_results: BTreeMap<String, CaseResult> = doc
            .results
            .iter()
            .map(|r| (r.case.clone(), r.clone()))
            .collect();

        // Applicability: for streams from tag-scheduling runners
        // (`scheduling` absent or anything but "none"), the reported
        // status must AGREE with the case's tags — an executed
        // non-applicable case is manifest/adapter drift and stays an
        // error. For execute-everything streams (`scheduling: none`,
        // e.g. the composed runner, which cannot see the tags section
        // — findings #14), this layer is the scheduler: executed
        // non-applicable cases are reclassified to not-applicable in
        // the host-embed shape, so downstream (matrix, has_failures,
        // differential comparison) sees runner parity.
        if let Some(missing) = missing {
            let scheduled = doc.envelope.scheduled();
            let mut reclassified = 0usize;
            let mut drift_smell = 0usize;
            for r in target_results.values_mut() {
                let Some(tags) = tags_for(&r.case) else {
                    continue; // coverage already flagged
                };
                let applies = tags.applies(missing);
                match (applies, r.status) {
                    (false, Status::NotApplicable) | (false, Status::NotReached) => {}
                    (false, _) if !scheduled => {
                        let mark = tags.excluding_mark(missing);
                        // A pass despite a *required* feature being
                        // declared missing smells like the manifest is
                        // wrong — preserve that scent of the strict
                        // gate as a warning.
                        if r.status == Status::Pass && mark.is_some_and(|m| !m.is_negative()) {
                            drift_smell += 1;
                        }
                        reclassified += 1;
                        r.status = Status::NotApplicable;
                        r.provenance = None;
                        r.detail = mark.map(|m| m.to_string());
                        r.seed = None;
                        r.duration_ms = None;
                        r.diagnostics.clear();
                        r.diagnostics_complete = true;
                    }
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
            if reclassified > 0 {
                let smell = if drift_smell > 0 {
                    format!(
                        "; {drift_smell} of them passed despite requiring a \
                         declared-missing feature (manifest drift?)"
                    )
                } else {
                    String::new()
                };
                warnings.push(format!(
                    "target `{target}`: {reclassified} case(s) executed by an \
                     unscheduled runner reclassified to not-applicable{smell}"
                ));
            }
        }

        results.insert(target.clone(), target_results);
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
    fn artifact_hash_binding() {
        let mut lock = corpus_lock();
        lock.suite.artifact_sha256 = Some("aa11".into());

        // Matching hashes: clean.
        let mut matching = native_doc();
        matching.envelope.suite.artifact_sha256 = Some("aa11".into());
        let mut sim_matching = sim_doc();
        sim_matching.envelope.suite.artifact_sha256 = Some("aa11".into());
        let agg = aggregate(
            &lock,
            &manifest(MANIFEST),
            &[
                ("native".into(), matching),
                ("sim".into(), sim_matching.clone()),
            ],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);

        // Mismatch: results from a different suite build are refused.
        let mut stale = native_doc();
        stale.envelope.suite.artifact_sha256 = Some("bb22".into());
        let agg = aggregate(
            &lock,
            &manifest(MANIFEST),
            &[("native".into(), stale), ("sim".into(), sim_matching)],
        );
        assert!(
            agg.errors
                .iter()
                .any(|e| e.contains("sha256") && e.contains("bb22") && e.contains("aa11")),
            "{:?}",
            agg.errors
        );

        // Envelopes without a hash are tolerated (foreign runners).
        let agg = aggregate(
            &lock,
            &manifest(MANIFEST),
            &[("native".into(), native_doc()), ("sim".into(), sim_doc())],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);
    }

    #[test]
    fn suite_name_mismatch_is_warning() {
        let mut d = native_doc();
        d.envelope.suite.name = "other".into();
        let agg = aggregate(
            &corpus_lock(),
            &manifest(MANIFEST),
            &[("native".into(), d), ("sim".into(), sim_doc())],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);
        assert!(
            agg.warnings
                .iter()
                .any(|w| w.contains("`other`") && w.contains("`suite`")),
            "{:?}",
            agg.warnings
        );
    }

    #[test]
    fn unknown_target_is_error() {
        // The undeclared target reports a full, plausible result set
        // (including a not-applicable tagged case): exactly ONE error
        // must come out of it — the root cause — not a per-case
        // applicability cascade judged against a fabricated
        // missing-set.
        let agg = aggregate(
            &corpus_lock(),
            &manifest(MANIFEST),
            &[
                ("native".into(), native_doc()),
                ("sim".into(), sim_doc()),
                ("mystery".into(), doc("mystery", sim_doc().results)),
            ],
        );
        let mystery: Vec<&String> = agg
            .errors
            .iter()
            .filter(|e| e.contains("mystery"))
            .collect();
        assert_eq!(mystery.len(), 1, "{:?}", agg.errors);
        assert!(mystery[0].contains("not declared"), "{:?}", agg.errors);
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

    /// #36 option (b): a stream declaring `scheduling: none` (the
    /// composed runner) gets applicability *applied* by this layer —
    /// executed non-applicable cases are reclassified to the
    /// host-embed not-applicable shape instead of erroring — while
    /// scheduled/legacy streams keep the strict gate (previous test).
    #[test]
    fn unscheduled_stream_gets_applicability_applied() {
        // The composed runner executes everything: on sim (hsm
        // missing) the hsm case "ran" (and passed — drift smell), and
        // the !hsm decline probe ran too.
        let mut d = doc(
            "sim",
            vec![
                result("suite/add", Status::Pass, None),
                result("suite/hsm/attest", Status::Pass, None),
                result("suite/hsm/declined", Status::Pass, None),
            ],
        );
        d.envelope.run.scheduling = Some("none".into());
        // native, also unscheduled: the !hsm decline probe executed
        // (and failed, as decline probes do where the feature exists).
        let mut n = doc(
            "native",
            vec![
                result("suite/add", Status::Pass, None),
                result("suite/hsm/attest", Status::Pass, None),
                result("suite/hsm/declined", Status::Fail, Some("feature present")),
            ],
        );
        n.envelope.run.scheduling = Some("none".into());

        let agg = aggregate(
            &corpus_lock(),
            &manifest(MANIFEST),
            &[("native".into(), n), ("sim".into(), d)],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);

        // Reclassified to exactly the host-embed shape: N/A with the
        // excluding mark as detail, nothing else.
        let sim_attest = &agg.results["sim"]["suite/hsm/attest"];
        assert_eq!(sim_attest.status, Status::NotApplicable);
        assert_eq!(sim_attest.detail.as_deref(), Some("hsm"));
        assert_eq!(sim_attest.provenance, None);
        assert!(sim_attest.diagnostics.is_empty());
        let native_declined = &agg.results["native"]["suite/hsm/declined"];
        assert_eq!(native_declined.status, Status::NotApplicable);
        assert_eq!(native_declined.detail.as_deref(), Some("!hsm"));

        // The reclassified fail no longer fails the corpus (that is
        // the point: runner parity), and the drift smell is preserved
        // as a warning on the stream where the required-feature case
        // passed anyway.
        assert!(!agg.has_failures(), "{:?}", agg.results);
        assert!(agg.ok());
        assert!(
            agg.warnings
                .iter()
                .any(|w| w.contains("sim") && w.contains("reclassified") && w.contains("drift")),
            "{:?}",
            agg.warnings
        );
        assert!(
            agg.warnings.iter().any(|w| w.contains("native")
                && w.contains("reclassified")
                && !w.contains("drift")),
            "{:?}",
            agg.warnings
        );
    }

    /// The reclassified corpus must be indistinguishable from a
    /// scheduled runner's corpus downstream.
    #[test]
    fn unscheduled_reclassification_matches_scheduled_results() {
        let mut unscheduled = doc(
            "sim",
            vec![
                result("suite/add", Status::Pass, None),
                result("suite/hsm/attest", Status::Fail, Some("no hsm here")),
                result("suite/hsm/declined", Status::Pass, None),
            ],
        );
        unscheduled.envelope.run.scheduling = Some("none".into());
        let mut agg_a = aggregate(
            &corpus_lock(),
            &manifest(MANIFEST),
            &[("native".into(), native_doc()), ("sim".into(), unscheduled)],
        );
        let agg_b = aggregate(
            &corpus_lock(),
            &manifest(MANIFEST),
            &[("native".into(), native_doc()), ("sim".into(), sim_doc())],
        );
        assert!(agg_a.errors.is_empty(), "{:?}", agg_a.errors);
        assert_eq!(agg_a.results["sim"], agg_b.results["sim"]);
        // Same rendered matrix once the reclassification warning (the
        // only intended difference) is set aside.
        agg_a.warnings.clear();
        assert_eq!(crate::matrix::render(&agg_a), crate::matrix::render(&agg_b));
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
    /// and manifest entry. Every partial-deletion state must fail
    /// validation; the complete deletion must pass.
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
