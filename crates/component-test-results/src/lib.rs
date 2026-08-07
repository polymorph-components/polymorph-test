//! The canonical results model (#26): event stream (JSONL edge
//! encoding) and document form, with the normative fold rules:
//! - an unterminated segment folds its open (selected but unreported)
//!   cases to `not-reached`
//! - unknown statuses fold to no-result + validation warning
//! - empty selection is a run error
//!
//! Deliberately a leaf crate (core + serde only) so *guest-side*
//! encoders can link it: every producer of the frozen wire format —
//! host runners, the composed wasi:cli runner core, future SDKs —
//! must share these types rather than hand-rolling JSON (#34).
//! Host-side consumers usually reach it through
//! `component_test_formats::results`, a re-export.

use std::collections::BTreeMap;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

/// Re-exported for encoders: the provenance vocabulary lives in core
/// (guest suites compile core anyway via the SDK).
pub use component_test_core::Provenance;

pub const RESULTS_VERSION: &str = "0.1";

/// First line of a stream / header of a document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope {
    /// Format version tag.
    #[serde(rename = "component-test-results")]
    pub version: String,
    /// Opaque target key (implementation × environment).
    pub target: String,
    pub suite: SuiteInfo,
    pub run: RunInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SuiteInfo {
    pub name: String,
    /// sha256 of the suite component the results were produced from,
    /// hex. Provenance, not identity: builds are not reproducible
    /// across environments, so consumers must not require equality
    /// with a hash recorded elsewhere (e.g. a committed lockfile's).
    /// `aggregate` uses it only for the reproducibility-independent
    /// check that targets aggregated together ran the same build.
    #[serde(
        default,
        rename = "artifact-sha256",
        skip_serializing_if = "Option::is_none"
    )]
    pub artifact_sha256: Option<String>,
    #[serde(
        default,
        rename = "lockfile-sha256",
        skip_serializing_if = "Option::is_none"
    )]
    pub lockfile_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RunInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(default)]
    pub segment: u32,
    /// How the producer selected cases (additive vocabulary, kept as a
    /// string so unknown future values parse):
    /// - `"tags"`: the runner applied feature-tag scheduling
    ///   (not-applicable cases were excluded, not executed)
    /// - `"none"`: execute-everything (e.g. the composed wasi:cli
    ///   runner, which cannot see the tags section — wac strips it)
    /// - absent: legacy stream; consumers must assume `"tags"` (the
    ///   strict direction: applicability drift stays an error).
    ///
    /// The aggregator uses this to decide between *policing*
    /// applicability (scheduled streams: an executed non-applicable
    /// case is manifest/adapter drift) and *applying* it (unscheduled
    /// streams: executed non-applicable cases are reclassified).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduling: Option<String>,
}

impl Envelope {
    /// A stream header with the format defaults: current version,
    /// segment 0, `"tags"` scheduling (the strict gate). Fields are
    /// public — set provenance (`suite.artifact_sha256`, `run.id`, …)
    /// directly on the value. Exists so producers stop hand-assembling
    /// the struct and silently diverging on the defaults.
    pub fn new(target: impl Into<String>, suite_name: impl Into<String>) -> Self {
        Envelope {
            version: RESULTS_VERSION.to_string(),
            target: target.into(),
            suite: SuiteInfo {
                name: suite_name.into(),
                ..SuiteInfo::default()
            },
            run: RunInfo {
                scheduling: Some("tags".to_string()),
                ..RunInfo::default()
            },
        }
    }

    /// Did the producer apply tag scheduling? Anything except an
    /// explicit `"none"` counts as scheduled — unknown future
    /// vocabulary defaults to the strict gate.
    pub fn scheduled(&self) -> bool {
        self.run.scheduling.as_deref() != Some("none")
    }
}

/// One line of the JSONL stream after the envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Event {
    /// Harness breakage — never a case result.
    RunError(RunError),
    /// A case's result.
    Case(CaseResult),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunError {
    #[serde(rename = "run-error")]
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaseResult {
    pub case: String,
    pub status: Status,
    /// Executed statuses only; how the verdict came to be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// fail/skipped payload, not-applicable mark, deselection reason,
    /// or not-reached cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Replay token (executed cases, when randomness is virtualized).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    #[serde(
        default,
        rename = "duration-ms",
        skip_serializing_if = "Option::is_none"
    )]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
    /// False when capped, or a prefix due to trap/hang abandonment.
    #[serde(default = "default_true", rename = "diagnostics-complete")]
    pub diagnostics_complete: bool,
}

fn default_true() -> bool {
    true
}

/// Case status (the wire vocabulary is [`Status::word`]).
///
/// `#[non_exhaustive]`: the results schema evolves additively (frozen
/// surface #3), so downstream matches must carry a wildcard arm.
/// Unknown statuses *on the wire* never deserialize to this enum —
/// [`fold_jsonl`] diverts them to `unknown_statuses`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Pass,
    Fail,
    Skipped,
    NotReached,
    NotApplicable,
    Deselected,
}

impl Status {
    pub fn executed(self) -> bool {
        matches!(self, Status::Pass | Status::Fail | Status::Skipped)
    }

    /// Does this status make a run/corpus failing? The single
    /// definition behind `Aggregate::has_failures`, the CLI's fold and
    /// aggregate exit codes, and the matrix Failures section.
    pub fn failing(self) -> bool {
        matches!(self, Status::Fail | Status::NotReached)
    }

    /// The kebab-case schema word for this status (the wire vocabulary;
    /// use it anywhere a status is shown to a human).
    pub fn word(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Skipped => "skipped",
            Status::NotReached => "not-reached",
            Status::NotApplicable => "not-applicable",
            Status::Deselected => "deselected",
        }
    }
}

/// The folded document form.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Document {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub results: Vec<CaseResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_errors: Vec<String>,
    /// Statuses this consumer did not understand (case → raw status).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_statuses: BTreeMap<String, String>,
    /// True unless the stream ended without a terminator.
    pub terminated: bool,
}

/// Tager event ending a segment cleanly.
pub const TERMINATOR: &str = r#"{"segment-end":true}"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SegmentEnd {
    #[serde(rename = "segment-end")]
    segment_end: bool,
}

/// Serialize the envelope + events + terminator as JSONL.
pub fn to_jsonl(envelope: &Envelope, events: &[Event]) -> anyhow::Result<String> {
    let mut out = serde_json::to_string(envelope)?;
    out.push('\n');
    for ev in events {
        out.push_str(&serde_json::to_string(ev)?);
        out.push('\n');
    }
    out.push_str(TERMINATOR);
    out.push('\n');
    Ok(out)
}

/// Fold a JSONL stream into a [`Document`], applying the normative
/// rules. `selected` is the scheduler's full selection for this segment
/// (used for the `not-reached` fold on unterminated streams); pass the
/// lockfile-ordered selected names.
pub fn fold_jsonl<S: AsRef<str>>(stream: &str, selected: &[S]) -> anyhow::Result<Document> {
    let mut lines = stream.lines().filter(|l| !l.trim().is_empty());
    let envelope: Envelope =
        serde_json::from_str(lines.next().context("empty stream: missing envelope")?)
            .context("parsing envelope")?;
    // The version tag is the schema's evolution gate: additive changes
    // keep the version, so an unknown version means the stream needs
    // something this consumer doesn't understand. Refuse loudly rather
    // than misfold (lockfile/manifest validators do the same).
    if envelope.version != RESULTS_VERSION {
        anyhow::bail!(
            "unsupported results version `{}` (this consumer understands `{RESULTS_VERSION}`)",
            envelope.version
        );
    }

    if selected.is_empty() {
        anyhow::bail!("empty selection is a run error");
    }

    let mut results: Vec<CaseResult> = Vec::new();
    let mut run_errors = Vec::new();
    let mut unknown_statuses = BTreeMap::new();
    let mut terminated = false;

    for line in lines {
        if terminated {
            anyhow::bail!("events after segment terminator");
        }
        if let Ok(end) = serde_json::from_str::<SegmentEnd>(line) {
            if end.segment_end {
                terminated = true;
                continue;
            }
        }
        // Tolerant status handling: parse loosely first.
        let raw: serde_json::Value = serde_json::from_str(line).context("parsing event line")?;
        if let Some(msg) = raw.get("run-error").and_then(|v| v.as_str()) {
            run_errors.push(msg.to_string());
            continue;
        }
        match serde_json::from_value::<CaseResult>(raw.clone()) {
            Ok(cr) => results.push(cr),
            Err(_) => {
                // Unknown status (or shape): record and continue.
                let case = raw
                    .get("case")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown>")
                    .to_string();
                let status = raw
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unparseable>")
                    .to_string();
                unknown_statuses.insert(case, status);
            }
        }
    }

    if !terminated {
        // Normative fold: open cases become not-reached, attributed to
        // the last executed case if it failed abnormally.
        let reported: std::collections::BTreeSet<String> = results
            .iter()
            .map(|r| r.case.clone())
            .chain(unknown_statuses.keys().cloned())
            .collect();
        let cause = results
            .iter()
            .rev()
            .find(|r| {
                r.status == Status::Fail
                    && matches!(
                        r.provenance,
                        Some(Provenance::Trap | Provenance::LimitExceeded(_))
                    )
            })
            .map(|r| r.case.clone());
        for name in selected {
            let name = name.as_ref();
            if !reported.contains(name) {
                results.push(CaseResult {
                    case: name.to_string(),
                    status: Status::NotReached,
                    provenance: None,
                    detail: cause.clone().map(|c| format!("segment ended after `{c}`")),
                    seed: None,
                    duration_ms: None,
                    diagnostics: vec![],
                    diagnostics_complete: true,
                });
            }
        }
    }

    Ok(Document {
        envelope,
        results,
        run_errors,
        unknown_statuses,
        terminated,
    })
}

/// Fold role-labelled result sets from one multi-instance run (a
/// two-party pair, an N-way mesh) into one event per case.
///
/// - **Worst status wins**: fail > not-reached > skipped > everything
///   else; ties go to the earlier role, and case order is the first
///   reporting role's stream order (later-role-only cases append in
///   their role's order).
/// - **Details and diagnostics are role-labelled** so a red cell names
///   the side that failed: the winner's first, the other non-passing
///   roles' appended in role order.
/// - A case only one role reported passes through **unmodified** — a
///   single report is not a disagreement to label.
/// - `diagnostics_complete` ANDs across roles; `duration_ms` is the
///   max reported.
pub fn fold_roles<S: AsRef<str>>(roles: &[(S, Vec<CaseResult>)]) -> Vec<CaseResult> {
    fn rank(status: Status) -> u8 {
        match status {
            Status::Fail => 3,
            Status::NotReached => 2,
            Status::Skipped => 1,
            _ => 0,
        }
    }

    // (role index, result) per case, in first-report order.
    let mut order: Vec<String> = Vec::new();
    let mut by_case: BTreeMap<String, Vec<(usize, CaseResult)>> = BTreeMap::new();
    for (idx, (_, results)) in roles.iter().enumerate() {
        for r in results {
            let entry = by_case.entry(r.case.clone()).or_default();
            if entry.is_empty() {
                order.push(r.case.clone());
            }
            entry.push((idx, r.clone()));
        }
    }

    let mut out = Vec::new();
    for case in order {
        let mut reports = by_case.remove(&case).unwrap_or_default();
        if reports.len() == 1 {
            out.push(reports.remove(0).1);
            continue;
        }
        // Stable max: strictly-greater keeps the earliest of a tie.
        let win_pos = reports.iter().enumerate().fold(0, |best, (i, (_, r))| {
            if rank(r.status) > rank(reports[best].1.status) {
                i
            } else {
                best
            }
        });
        let (win_idx, mut winner) = reports.remove(win_pos);
        let winner_role = roles[win_idx].0.as_ref();

        if rank(winner.status) > 0 {
            let mut detail = format!(
                "{winner_role}: {}",
                winner.detail.as_deref().unwrap_or(winner.status.word())
            );
            for (idx, r) in &reports {
                if rank(r.status) > 0 {
                    detail.push_str(&format!(
                        "; {}: {}",
                        roles[*idx].0.as_ref(),
                        r.detail.as_deref().unwrap_or(r.status.word())
                    ));
                }
            }
            winner.detail = Some(detail);
        }

        let mut diagnostics: Vec<String> = winner
            .diagnostics
            .iter()
            .map(|d| format!("{winner_role}: {d}"))
            .collect();
        for (idx, r) in &reports {
            diagnostics.extend(
                r.diagnostics
                    .iter()
                    .map(|d| format!("{}: {d}", roles[*idx].0.as_ref())),
            );
        }
        winner.diagnostics = diagnostics;
        winner.diagnostics_complete =
            winner.diagnostics_complete && reports.iter().all(|(_, r)| r.diagnostics_complete);
        winner.duration_ms = reports
            .iter()
            .filter_map(|(_, r)| r.duration_ms)
            .chain(winner.duration_ms)
            .max();
        out.push(winner);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role_case(case: &str, status: Status, detail: Option<&str>) -> CaseResult {
        CaseResult {
            case: case.into(),
            status,
            provenance: None,
            detail: detail.map(Into::into),
            seed: None,
            duration_ms: None,
            diagnostics: Vec::new(),
            diagnostics_complete: true,
        }
    }

    #[test]
    fn envelope_new_carries_the_format_defaults() {
        let e = Envelope::new("wasmtime/pair", "suite_ct");
        assert_eq!(e.version, RESULTS_VERSION);
        assert_eq!(e.run.segment, 0);
        assert_eq!(e.run.scheduling.as_deref(), Some("tags"));
        assert!(e.scheduled());
        assert_eq!(e.suite.name, "suite_ct");
        assert!(e.suite.artifact_sha256.is_none());
    }

    #[test]
    fn fold_roles_worst_status_wins_with_role_labels() {
        let offerer = vec![role_case("pair/echo", Status::Pass, None)];
        let answerer = vec![role_case("pair/echo", Status::Fail, Some("boom"))];
        let folded = fold_roles(&[("offerer", offerer), ("answerer", answerer)]);
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].status, Status::Fail);
        // The winner's label leads even when the winner is a later role.
        assert_eq!(folded[0].detail.as_deref(), Some("answerer: boom"));
    }

    #[test]
    fn fold_roles_both_failing_names_both_sides() {
        let offerer = vec![role_case("pair/echo", Status::NotReached, None)];
        let answerer = vec![role_case("pair/echo", Status::Fail, Some("boom"))];
        let folded = fold_roles(&[("offerer", offerer), ("answerer", answerer)]);
        assert_eq!(
            folded[0].detail.as_deref(),
            Some("answerer: boom; offerer: not-reached")
        );
    }

    #[test]
    fn fold_roles_tie_goes_to_the_earlier_role() {
        let offerer = vec![role_case("pair/echo", Status::Fail, Some("off"))];
        let answerer = vec![role_case("pair/echo", Status::Fail, Some("ans"))];
        let folded = fold_roles(&[("offerer", offerer), ("answerer", answerer)]);
        assert_eq!(
            folded[0].detail.as_deref(),
            Some("offerer: off; answerer: ans")
        );
    }

    #[test]
    fn fold_roles_single_report_passes_through_unlabelled() {
        let offerer = vec![role_case("pair/only-off", Status::Fail, Some("boom"))];
        let folded = fold_roles(&[("offerer", offerer), ("answerer", Vec::new())]);
        assert_eq!(folded[0].detail.as_deref(), Some("boom"));
    }

    #[test]
    fn fold_roles_order_is_first_report_order() {
        let offerer = vec![
            role_case("pair/a", Status::Pass, None),
            role_case("pair/b", Status::Pass, None),
        ];
        let answerer = vec![
            role_case("pair/b", Status::Pass, None),
            role_case("pair/z", Status::Pass, None),
        ];
        let folded = fold_roles(&[("offerer", offerer), ("answerer", answerer)]);
        let order: Vec<&str> = folded.iter().map(|r| r.case.as_str()).collect();
        assert_eq!(order, vec!["pair/a", "pair/b", "pair/z"]);
    }

    #[test]
    fn fold_roles_three_way_diagnostics_and_duration() {
        let mut a = role_case("mesh/x", Status::Pass, None);
        a.diagnostics = vec!["hello".into()];
        a.duration_ms = Some(5);
        let mut b = role_case("mesh/x", Status::Fail, Some("mid"));
        b.duration_ms = Some(9);
        b.diagnostics_complete = false;
        let c = role_case("mesh/x", Status::Skipped, Some("late"));
        let folded = fold_roles(&[("a", vec![a]), ("b", vec![b]), ("c", vec![c])]);
        assert_eq!(folded[0].status, Status::Fail);
        assert_eq!(folded[0].detail.as_deref(), Some("b: mid; c: late"));
        assert_eq!(folded[0].diagnostics, vec!["a: hello".to_string()]);
        assert!(!folded[0].diagnostics_complete);
        assert_eq!(folded[0].duration_ms, Some(9));
    }

    fn envelope() -> Envelope {
        Envelope {
            version: RESULTS_VERSION.into(),
            target: "wasmtime/linux".into(),
            suite: SuiteInfo {
                name: "sample".into(),
                ..Default::default()
            },
            run: RunInfo::default(),
        }
    }

    fn pass(case: &str) -> Event {
        Event::Case(CaseResult {
            case: case.into(),
            status: Status::Pass,
            provenance: Some(Provenance::Returned),
            detail: None,
            seed: None,
            duration_ms: Some(1),
            diagnostics: vec![],
            diagnostics_complete: true,
        })
    }

    #[test]
    fn roundtrip_terminated() {
        let events = vec![pass("a/x"), pass("a/y")];
        let jsonl = to_jsonl(&envelope(), &events).unwrap();
        let doc = fold_jsonl(&jsonl, &["a/x", "a/y"]).unwrap();
        assert!(doc.terminated);
        assert_eq!(doc.results.len(), 2);
        assert!(doc.run_errors.is_empty());
    }

    #[test]
    fn unterminated_folds_not_reached() {
        let mut trap = match pass("a/x") {
            Event::Case(mut c) => {
                c.status = Status::Fail;
                c.provenance = Some(Provenance::Trap);
                c.detail = Some("wasm trap: unreachable".into());
                c.diagnostics_complete = false;
                Event::Case(c)
            }
            _ => unreachable!(),
        };
        // stream without terminator: envelope + one trap-fail
        let mut jsonl = serde_json::to_string(&envelope()).unwrap();
        jsonl.push('\n');
        if let Event::Case(c) = &mut trap {
            jsonl.push_str(&serde_json::to_string(&c).unwrap());
            jsonl.push('\n');
        }
        let doc = fold_jsonl(&jsonl, &["a/x", "a/y", "a/z"]).unwrap();
        assert!(!doc.terminated);
        assert_eq!(doc.results.len(), 3);
        let nr: Vec<_> = doc
            .results
            .iter()
            .filter(|r| r.status == Status::NotReached)
            .collect();
        assert_eq!(nr.len(), 2);
        assert!(nr[0].detail.as_ref().unwrap().contains("a/x"));
    }

    #[test]
    fn unknown_status_tolerated() {
        let mut jsonl = serde_json::to_string(&envelope()).unwrap();
        jsonl.push('\n');
        jsonl.push_str(r#"{"case":"a/x","status":"abandoned-by-co-tenant"}"#);
        jsonl.push('\n');
        jsonl.push_str(r#"{"case":"a/y","status":"pass"}"#);
        jsonl.push('\n');
        jsonl.push_str(TERMINATOR);
        let doc = fold_jsonl(&jsonl, &["a/x", "a/y"]).unwrap();
        assert_eq!(
            doc.unknown_statuses.get("a/x").unwrap(),
            "abandoned-by-co-tenant"
        );
        assert_eq!(doc.results.len(), 1);
    }

    #[test]
    fn empty_selection_is_run_error() {
        let jsonl = to_jsonl(&envelope(), &[]).unwrap();
        assert!(fold_jsonl::<&str>(&jsonl, &[]).is_err());
    }

    #[test]
    fn run_error_events() {
        let mut jsonl = serde_json::to_string(&envelope()).unwrap();
        jsonl.push('\n');
        jsonl.push_str(r#"{"run-error":"enumeration trapped"}"#);
        jsonl.push('\n');
        jsonl.push_str(TERMINATOR);
        let doc = fold_jsonl(&jsonl, &["a/x"]).unwrap();
        assert_eq!(doc.run_errors, vec!["enumeration trapped"]);
    }

    #[test]
    fn unknown_version_refused() {
        let mut env = envelope();
        env.version = "9.9".into();
        let jsonl = to_jsonl(&env, &[pass("a/x")]).unwrap();
        let err = fold_jsonl(&jsonl, &["a/x"]).unwrap_err().to_string();
        assert!(err.contains("unsupported results version `9.9`"), "{err}");
    }

    /// Pins the wire vocabulary (frozen surface: additive evolution
    /// only). A word changing here is a schema break; a new variant
    /// must be *added* to these tables, never renamed.
    #[test]
    fn wire_vocabulary_pinned() {
        let statuses = [
            (Status::Pass, "pass"),
            (Status::Fail, "fail"),
            (Status::Skipped, "skipped"),
            (Status::NotReached, "not-reached"),
            (Status::NotApplicable, "not-applicable"),
            (Status::Deselected, "deselected"),
        ];
        for (status, word) in statuses {
            assert_eq!(status.word(), word);
            assert_eq!(
                serde_json::to_value(status).unwrap(),
                serde_json::Value::String(word.into())
            );
            assert_eq!(
                serde_json::from_value::<Status>(serde_json::Value::String(word.into())).unwrap(),
                status
            );
        }
        // Provenance: unit variants are flat strings; limit-exceeded
        // (first emitted by the case budgets, #45) is externally
        // tagged with an open payload vocabulary naming the limit.
        let provenances = [
            (Provenance::Returned, r#""returned""#),
            (Provenance::Trap, r#""trap""#),
            (
                Provenance::LimitExceeded("execution-budget".into()),
                r#"{"limit-exceeded":"execution-budget"}"#,
            ),
            (
                Provenance::LimitExceeded("case-timeout".into()),
                r#"{"limit-exceeded":"case-timeout"}"#,
            ),
        ];
        for (provenance, wire) in provenances {
            assert_eq!(serde_json::to_string(&provenance).unwrap(), wire);
            assert_eq!(
                serde_json::from_str::<Provenance>(wire).unwrap(),
                provenance
            );
        }
    }

    #[test]
    fn scheduling_field_semantics() {
        // Absent (legacy) and "tags" are scheduled; only an explicit
        // "none" opts a stream out of the strict applicability gate;
        // unknown future vocabulary stays strict.
        let mut env = envelope();
        assert!(env.scheduled());
        env.run.scheduling = Some("tags".into());
        assert!(env.scheduled());
        env.run.scheduling = Some("none".into());
        assert!(!env.scheduled());
        env.run.scheduling = Some("phase-of-moon".into());
        assert!(env.scheduled());

        // Wire form + legacy tolerance both directions.
        env.run.scheduling = Some("none".into());
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains(r#""scheduling":"none""#), "{json}");
        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, env);
        let legacy = r#"{"component-test-results":"0.1","target":"t","suite":{"name":"s"},"run":{"segment":0}}"#;
        let parsed: Envelope = serde_json::from_str(legacy).unwrap();
        assert!(parsed.scheduled());
        assert_eq!(parsed.run.scheduling, None);
    }
}
