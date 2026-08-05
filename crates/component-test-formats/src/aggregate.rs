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
    /// target → case → assessment against the target's expected-fail
    /// declarations (#48). Only declared cases appear here.
    pub assessments: BTreeMap<String, BTreeMap<String, Assessment>>,
    /// Validation failures.
    pub errors: Vec<String>,
    /// Tolerated oddities (unknown statuses).
    pub warnings: Vec<String>,
}

/// Host-side assessment of a reported result against a target's
/// `expected-fail` declarations (#48). Never a wire status: the stream
/// still carries `fail`/`pass`; this is the aggregation layer's
/// judgment of it, made where the manifest joins the results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assessment {
    /// Declared expected-fail and it did fail: tracked debt, excluded
    /// from corpus failure.
    ExpectedFail { reason: String, tracking: String },
    /// Declared expected-fail but it passed: the declaration is stale
    /// (also a validation error — the pass forces cleanup).
    UnexpectedPass,
}

impl Aggregate {
    /// True when validation passed and no case failed or was left
    /// unreached.
    pub fn ok(&self) -> bool {
        self.errors.is_empty() && !self.has_failures()
    }

    /// Is this result a corpus failure? `fail`/`not-reached`, except a
    /// fail assessed expected-fail (tracked debt). The single owner of
    /// the predicate: `has_failures`, the CLI exit path, and the
    /// matrix Failures section all route through it.
    pub fn result_failing(&self, target: &str, r: &CaseResult) -> bool {
        r.status.failing()
            && !matches!(
                self.assessments.get(target).and_then(|m| m.get(&r.case)),
                Some(Assessment::ExpectedFail { .. })
            )
    }

    /// Any corpus failure across all targets.
    pub fn has_failures(&self) -> bool {
        self.results
            .iter()
            .flat_map(|(target, m)| m.values().map(move |r| (target.as_str(), r)))
            .any(|(target, r)| self.result_failing(target, r))
    }

    /// Total expected-fail assessments (for summaries).
    pub fn expected_fail_count(&self) -> usize {
        self.assessments
            .values()
            .flat_map(|m| m.values())
            .filter(|a| matches!(a, Assessment::ExpectedFail { .. }))
            .count()
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
    let mut assessments: BTreeMap<String, BTreeMap<String, Assessment>> = BTreeMap::new();
    let mut seen_targets = BTreeSet::new();
    // (target, envelope artifact sha256) for the cross-target agreement
    // warning — collected during the per-target pass, judged after it.
    let mut envelope_hashes: Vec<(String, String)> = Vec::new();
    // Per-target generated-leaf sets, for the cross-target agreement
    // check after the loop.
    let mut generated_leaves: Vec<(String, BTreeSet<String>)> = Vec::new();

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
        // Artifact hashes are provenance, not identity: suite builds
        // are not reproducible across environments (compilers embed
        // source paths etc.), so envelope-vs-lockfile hash equality is
        // deliberately NOT required — the inventory checks below are
        // the authoritative binding. What the hashes do support is the
        // reproducibility-independent same-pipeline check: every target
        // in one aggregation should have run the same build (see the
        // cross-target agreement warning after this loop).
        if let Some(reported) = doc.envelope.suite.artifact_sha256.as_deref() {
            envelope_hashes.push((target.clone(), reported.to_string()));
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
        generated_leaves.push((
            target.clone(),
            doc.results
                .iter()
                .filter(|r| lockfile.prefix_of(&r.case).is_some())
                .map(|r| r.case.clone())
                .collect(),
        ));

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

        // Expected-fail declarations (#48): known failures are tracked
        // debt, never deleted tests. A declared case that failed is
        // assessed expected-fail (excluded from corpus failure); one
        // that passed is an unexpected pass — a validation error, so a
        // fixed case forces its declaration's cleanup. Stale
        // declarations (unknown case, or not applicable on this
        // target) are errors, the same discipline as every other
        // manifest cross-check. Runs after the scheduling
        // reclassification above so unscheduled streams are judged on
        // their normalized statuses.
        if let (Some(missing), Some(decl)) = (missing, manifest.targets.get(target)) {
            let mut target_assessments: BTreeMap<String, Assessment> = BTreeMap::new();
            for xf in &decl.expected_fail {
                let Some(tags) = tags_for(&xf.case) else {
                    errors.push(format!(
                        "target `{target}`: expected-fail declaration for `{}` names \
                         no known case (not in the lockfile, not under a generated \
                         prefix) — stale declaration?",
                        xf.case
                    ));
                    continue;
                };
                if !tags.applies(missing) {
                    errors.push(format!(
                        "target `{target}`: expected-fail declaration for `{}` is \
                         stale: the case is not applicable given missing-features \
                         {missing:?}",
                        xf.case
                    ));
                    continue;
                }
                match target_results.get(&xf.case).map(|r| r.status) {
                    Some(Status::Fail) => {
                        target_assessments.insert(
                            xf.case.clone(),
                            Assessment::ExpectedFail {
                                reason: xf.reason.clone(),
                                tracking: xf.tracking.clone(),
                            },
                        );
                    }
                    Some(Status::Pass) => {
                        target_assessments.insert(xf.case.clone(), Assessment::UnexpectedPass);
                        errors.push(format!(
                            "target `{target}`: expected-fail case `{}` passed — \
                             remove the stale declaration (reason was: {}; tracking: \
                             {})",
                            xf.case, xf.reason, xf.tracking
                        ));
                    }
                    Some(Status::Skipped) => warnings.push(format!(
                        "target `{target}`: expected-fail case `{}` was skipped: the \
                         declared failure went unexercised",
                        xf.case
                    )),
                    // not-reached stays a corpus failure (harness
                    // breakage is not the declared failure); the
                    // not-applicable shape was rejected statically
                    // above.
                    Some(_) => {}
                    None => warnings.push(format!(
                        "target `{target}`: expected-fail case `{}` did not \
                         materialize in this run — the declared failure went \
                         unexercised",
                        xf.case
                    )),
                }
            }
            if !target_assessments.is_empty() {
                assessments.insert(target.clone(), target_assessments);
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

    // Cross-target artifact agreement (opportunistic, warning only):
    // targets aggregated together normally ran the same suite build, so
    // envelopes that carry hashes should agree with each other. Unlike
    // envelope-vs-lockfile equality this needs no build reproducibility
    // — the artifacts came from one pipeline. Disagreement usually
    // means the pipeline mixed builds (stale artifact on one target).
    {
        let distinct: BTreeSet<&str> = envelope_hashes.iter().map(|(_, h)| h.as_str()).collect();
        if distinct.len() > 1 {
            let detail: Vec<String> = envelope_hashes
                .iter()
                .map(|(t, h)| format!("{t}={h}"))
                .collect();
            warnings.push(format!(
                "targets report differing suite artifact hashes (mixed builds?): {}",
                detail.join(", ")
            ));
        }
    }

    // Cross-target generated-leaf agreement: an enumeration-free
    // `[[generated]]` lockfile entry pins its prefix, not leaves, so
    // per-target coverage imposes no membership on such rows (#49;
    // rows generated with `lock --leaves` get exact per-target
    // coverage from `check_coverage` instead). But every target in one
    // aggregation ran the same suite artifact, whose enumeration is
    // deterministic, and scheduling emits not-applicable rows rather
    // than omitting cases — so the generated leaf *sets* must agree
    // across targets. A target missing leaves the others report has
    // silently shed rows (a filtered/truncated stream, or a runner
    // that omits rather than declines). For enumeration-free rows this
    // cannot catch a uniform regression (every target losing the same
    // rows); that bound is exactly what `--leaves` pinning adds.
    {
        let union: BTreeSet<&str> = generated_leaves
            .iter()
            .flat_map(|(_, s)| s.iter().map(|c| c.as_str()))
            .collect();
        for (target, leaves) in &generated_leaves {
            let missing: Vec<&str> = union
                .iter()
                .filter(|c| !leaves.contains(**c))
                .copied()
                .collect();
            if !missing.is_empty() {
                let shown = missing.iter().take(5).copied().collect::<Vec<_>>();
                let more = missing.len() - shown.len();
                let suffix = if more > 0 {
                    format!(" (+{more} more)")
                } else {
                    String::new()
                };
                errors.push(format!(
                    "target `{target}`: generated rows missing {} leaf case(s) \
                     other targets report: {}{suffix}",
                    missing.len(),
                    shown.join(", ")
                ));
            }
        }
    }

    Aggregate {
        targets: manifest.targets.keys().cloned().collect(),
        results,
        assessments,
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
    fn artifact_hashes_are_provenance_not_identity() {
        // Suite builds are not reproducible across environments, so a
        // lockfile hash differing from every envelope hash is the
        // expected-normal state (committed lock, CI-built artifact):
        // neither an error nor a warning.
        let mut lock = corpus_lock();
        lock.suite.artifact_sha256 = Some("aa11".into());
        let mut native = native_doc();
        native.envelope.suite.artifact_sha256 = Some("bb22".into());
        let mut sim = sim_doc();
        sim.envelope.suite.artifact_sha256 = Some("bb22".into());
        let agg = aggregate(
            &lock,
            &manifest(MANIFEST),
            &[("native".into(), native), ("sim".into(), sim)],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);
        assert!(agg.warnings.is_empty(), "{:?}", agg.warnings);

        // Cross-target disagreement IS surfaced (warning): targets in
        // one aggregation ran one pipeline, so differing hashes mean
        // mixed builds — a check that needs no reproducibility.
        let mut native = native_doc();
        native.envelope.suite.artifact_sha256 = Some("bb22".into());
        let mut sim = sim_doc();
        sim.envelope.suite.artifact_sha256 = Some("cc33".into());
        let agg = aggregate(
            &lock,
            &manifest(MANIFEST),
            &[("native".into(), native), ("sim".into(), sim)],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);
        assert!(
            agg.warnings
                .iter()
                .any(|w| w.contains("mixed builds") && w.contains("bb22") && w.contains("cc33")),
            "{:?}",
            agg.warnings
        );

        // Envelopes without a hash are tolerated (foreign runners) and
        // don't count toward agreement.
        let agg = aggregate(
            &lock,
            &manifest(MANIFEST),
            &[("native".into(), native_doc()), ("sim".into(), sim_doc())],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);
        assert!(agg.warnings.is_empty(), "{:?}", agg.warnings);
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

    const XFAIL_MANIFEST: &str = r#"
        version = "0.1"
        [features.hsm]
        kind = "gated"
        [targets.native]
        missing-features = []
        [targets.sim]
        missing-features = ["hsm"]
        [[targets.sim.expected-fail]]
        case = "suite/add"
        reason = "integer add broken on the sim backend"
        tracking = "https://example.test/issues/9"
    "#;

    /// #48 happy path: a declared known failure fails — tracked debt,
    /// not a corpus failure.
    #[test]
    fn expected_fail_excludes_known_failure() {
        let mut sim = sim_doc();
        sim.results[0] = result("suite/add", Status::Fail, Some("1 + 1 = 3"));
        let agg = aggregate(
            &corpus_lock(),
            &manifest(XFAIL_MANIFEST),
            &[("native".into(), native_doc()), ("sim".into(), sim)],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);
        assert!(!agg.has_failures(), "{:?}", agg.results);
        assert!(agg.ok());
        assert_eq!(agg.expected_fail_count(), 1);
        assert!(matches!(
            agg.assessments["sim"]["suite/add"],
            Assessment::ExpectedFail { .. }
        ));
        // The wire status is untouched: reclassification is judgment,
        // not rewriting.
        assert_eq!(agg.results["sim"]["suite/add"].status, Status::Fail);
    }

    /// #48 forced cleanup: the declared failure passed, so the stale
    /// declaration is a validation error.
    #[test]
    fn unexpected_pass_is_error() {
        let agg = aggregate(
            &corpus_lock(),
            &manifest(XFAIL_MANIFEST),
            &[("native".into(), native_doc()), ("sim".into(), sim_doc())],
        );
        assert!(
            agg.errors.iter().any(|e| e.contains("`suite/add`")
                && e.contains("passed")
                && e.contains("https://example.test/issues/9")),
            "{:?}",
            agg.errors
        );
        assert!(!agg.ok());
        assert_eq!(
            agg.assessments["sim"]["suite/add"],
            Assessment::UnexpectedPass
        );
        assert_eq!(agg.expected_fail_count(), 0);
    }

    /// #48 stale declarations: unknown case, and a case not applicable
    /// on the declaring target.
    #[test]
    fn stale_expected_fail_declarations_are_errors() {
        let unknown = manifest(
            r#"
            version = "0.1"
            [features.hsm]
            kind = "gated"
            [targets.native]
            missing-features = []
            [targets.sim]
            missing-features = ["hsm"]
            [[targets.sim.expected-fail]]
            case = "suite/no-such-case"
            reason = "r"
            tracking = "t"
        "#,
        );
        let agg = aggregate(
            &corpus_lock(),
            &unknown,
            &[("native".into(), native_doc()), ("sim".into(), sim_doc())],
        );
        assert!(
            agg.errors
                .iter()
                .any(|e| e.contains("suite/no-such-case") && e.contains("no known case")),
            "{:?}",
            agg.errors
        );

        // suite/hsm/attest requires hsm, which sim declares missing:
        // an xfail for it on sim can never be exercised.
        let not_applicable = manifest(
            r#"
            version = "0.1"
            [features.hsm]
            kind = "gated"
            [targets.native]
            missing-features = []
            [targets.sim]
            missing-features = ["hsm"]
            [[targets.sim.expected-fail]]
            case = "suite/hsm/attest"
            reason = "r"
            tracking = "t"
        "#,
        );
        let agg = aggregate(
            &corpus_lock(),
            &not_applicable,
            &[("native".into(), native_doc()), ("sim".into(), sim_doc())],
        );
        assert!(
            agg.errors
                .iter()
                .any(|e| e.contains("suite/hsm/attest") && e.contains("not applicable")),
            "{:?}",
            agg.errors
        );
    }

    /// #48 decision 2: a skipped expected-fail case neither satisfies
    /// nor violates the declaration — warning, not error.
    #[test]
    fn skipped_expected_fail_warns() {
        let mut sim = sim_doc();
        sim.results[0] = result("suite/add", Status::Skipped, Some("no backend"));
        let agg = aggregate(
            &corpus_lock(),
            &manifest(XFAIL_MANIFEST),
            &[("native".into(), native_doc()), ("sim".into(), sim)],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);
        assert!(
            agg.warnings
                .iter()
                .any(|w| w.contains("`suite/add`") && w.contains("unexercised")),
            "{:?}",
            agg.warnings
        );
        assert_eq!(agg.expected_fail_count(), 0);
    }

    /// #48: a declaration naming a generated leaf that never
    /// materialized is unexercised — warning (coverage does not require
    /// generated leaves, so nothing else would flag it).
    #[test]
    fn unmaterialized_expected_fail_warns() {
        let mut lock = corpus_lock();
        lock.generated.push(crate::lockfile::GeneratedEntry {
            prefix: "suite/gen".into(),
            tags: vec![],
            cases: vec![],
        });
        let xfail_gen = manifest(
            r#"
            version = "0.1"
            [features.hsm]
            kind = "gated"
            [targets.native]
            missing-features = []
            [targets.sim]
            missing-features = ["hsm"]
            [[targets.sim.expected-fail]]
            case = "suite/gen/tc999"
            reason = "r"
            tracking = "t"
        "#,
        );
        let agg = aggregate(
            &lock,
            &xfail_gen,
            &[("native".into(), native_doc()), ("sim".into(), sim_doc())],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);
        assert!(
            agg.warnings
                .iter()
                .any(|w| w.contains("suite/gen/tc999") && w.contains("did not materialize")),
            "{:?}",
            agg.warnings
        );
    }

    /// #49: `[[generated]]` pins prefixes, not leaves, so per-target
    /// coverage imposes no membership on generated rows — but targets
    /// in one aggregation ran the same artifact, so their generated
    /// leaf sets must agree. A target missing leaves the others report
    /// has silently shed rows.
    #[test]
    fn generated_leaf_sets_must_agree_across_targets() {
        let mut lock = corpus_lock();
        lock.generated.push(crate::lockfile::GeneratedEntry {
            prefix: "suite/gen".into(),
            tags: vec![],
            cases: vec![],
        });
        let full = |target: &str| {
            let mut results = match target {
                "native" => native_doc().results,
                _ => sim_doc().results,
            };
            results.push(result("suite/gen/tc1", Status::Pass, None));
            results.push(result("suite/gen/tc2", Status::Pass, None));
            doc(target, results)
        };

        // Agreement: no errors.
        let agg = aggregate(
            &lock,
            &manifest(MANIFEST),
            &[
                ("native".into(), full("native")),
                ("sim".into(), full("sim")),
            ],
        );
        assert!(agg.errors.is_empty(), "{:?}", agg.errors);

        // One target sheds a leaf: error names the target and the leaf.
        let mut shed = full("sim");
        shed.results.retain(|r| r.case != "suite/gen/tc2");
        let agg = aggregate(
            &lock,
            &manifest(MANIFEST),
            &[("native".into(), full("native")), ("sim".into(), shed)],
        );
        assert!(
            agg.errors.iter().any(|e| e.contains("target `sim`")
                && e.contains("generated rows missing 1 leaf case(s)")
                && e.contains("suite/gen/tc2")),
            "{:?}",
            agg.errors
        );
        // The intact target is not implicated.
        assert!(
            !agg.errors
                .iter()
                .any(|e| e.contains("target `native`") && e.contains("generated rows missing")),
            "{:?}",
            agg.errors
        );

        // A uniform drop (both targets lose the same leaf) is NOT
        // caught here — the documented limit; the lockfile-side leaf
        // pin (#49) owns that bound.
        let mut a = full("native");
        a.results.retain(|r| r.case != "suite/gen/tc2");
        let mut b = full("sim");
        b.results.retain(|r| r.case != "suite/gen/tc2");
        let agg = aggregate(
            &lock,
            &manifest(MANIFEST),
            &[("native".into(), a), ("sim".into(), b)],
        );
        assert!(
            !agg.errors
                .iter()
                .any(|e| e.contains("generated rows missing")),
            "{:?}",
            agg.errors
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
