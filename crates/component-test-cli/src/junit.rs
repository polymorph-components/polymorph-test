//! JUnit XML emission from folded results streams: one converter from
//! the canonical format (#11), so every runner — host-embed, composed,
//! jco, browser — gets CI-native test reporting through the format it
//! already speaks, rather than per-runner emitters.
//!
//! Mapping (JUnit has a smaller vocabulary than the results schema; the
//! rule is *never greener than the stream*):
//! - one `<testsuite>` per target, `<testcase classname>` = the case
//!   name up to its last segment, `name` = the leaf segment;
//! - `pass` → plain testcase; `fail` → `<failure>` (message = detail,
//!   type = provenance); `not-reached` → `<failure type="not-reached">`
//!   (JUnit has no "the runner never got there", and it is failing in
//!   this schema);
//! - `skipped` → `<skipped>`; `not-applicable` / `deselected` →
//!   `<skipped>` with the mark/reason in the message (JUnit has no
//!   scheduling vocabulary; skipped is the honest downgrade);
//! - diagnostics ride `<system-out>`, flagged when incomplete;
//! - run errors, unterminated segments, and unknown wire statuses
//!   become `<error>`-status testcases: a converter must surface
//!   transport problems, not launder them.
//!
//! This is a converter, not a gate: it always exits successfully after
//! writing (failures are *in* the XML, where the consuming UI wants
//! them); `fold`/`aggregate` remain the exit-code authorities.
//! Expected-fail assessment is aggregate vocabulary (it needs the
//! manifest) and deliberately absent here: streams carry raw verdicts.

use component_test_formats::results::{Document, Status};

/// Render one `<testsuites>` document from per-target folded streams.
pub fn junit(streams: &[(String, Document)]) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n");
    for (target, doc) in streams {
        out.push_str(&suite(target, doc));
    }
    out.push_str("</testsuites>\n");
    out
}

fn suite(target: &str, doc: &Document) -> String {
    let mut cases = String::new();
    let mut tests = 0usize;
    let mut failures = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;
    let mut total_ms = 0u64;

    for r in &doc.results {
        tests += 1;
        total_ms += r.duration_ms.unwrap_or(0);
        let (classname, name) = split_case(&r.case);
        let time = format!("{:.3}", r.duration_ms.unwrap_or(0) as f64 / 1000.0);
        let open = format!(
            "    <testcase classname=\"{}\" name=\"{}\" time=\"{time}\"",
            attr(classname),
            attr(name)
        );
        let detail = r.detail.as_deref().unwrap_or("");
        let body = match r.status {
            Status::Pass => String::new(),
            Status::Fail => {
                failures += 1;
                let kind = match &r.provenance {
                    Some(component_test_formats::results::Provenance::Trap) => "trap".into(),
                    Some(component_test_formats::results::Provenance::LimitExceeded(l)) => {
                        format!("limit-exceeded: {l}")
                    }
                    _ => "fail".into(),
                };
                format!(
                    "      <failure message=\"{}\" type=\"{}\"/>\n",
                    attr(detail),
                    attr(&kind)
                )
            }
            Status::NotReached => {
                failures += 1;
                format!(
                    "      <failure message=\"{}\" type=\"not-reached\"/>\n",
                    attr(detail)
                )
            }
            Status::Skipped => {
                skipped += 1;
                format!("      <skipped message=\"{}\"/>\n", attr(detail))
            }
            Status::NotApplicable => {
                skipped += 1;
                format!(
                    "      <skipped message=\"not applicable ({})\"/>\n",
                    attr(detail)
                )
            }
            Status::Deselected => {
                skipped += 1;
                format!(
                    "      <skipped message=\"deselected ({})\"/>\n",
                    attr(detail)
                )
            }
            // Future additive statuses (#[non_exhaustive]): never
            // greener than the stream.
            _ => {
                failures += 1;
                format!(
                    "      <failure message=\"{}\" type=\"{}\"/>\n",
                    attr(detail),
                    attr(r.status.word())
                )
            }
        };
        let sysout = if r.diagnostics.is_empty() {
            String::new()
        } else {
            let mut s = r.diagnostics.join("\n");
            if !r.diagnostics_complete {
                s.push_str("\n[diagnostics incomplete]");
            }
            format!("      <system-out>{}</system-out>\n", text(&s))
        };
        if body.is_empty() && sysout.is_empty() {
            cases.push_str(&open);
            cases.push_str("/>\n");
        } else {
            cases.push_str(&open);
            cases.push_str(">\n");
            cases.push_str(&body);
            cases.push_str(&sysout);
            cases.push_str("    </testcase>\n");
        }
    }

    // Transport problems: surfaced as error-status testcases, never
    // dropped.
    let mut synthetic = |name: &str, kind: &str, msg: &str, errors: &mut usize| {
        *errors += 1;
        tests += 1;
        cases.push_str(&format!(
            "    <testcase classname=\"{}\" name=\"{}\" time=\"0.000\">\n      \
             <error message=\"{}\" type=\"{}\"/>\n    </testcase>\n",
            attr(target),
            attr(name),
            attr(msg),
            attr(kind)
        ));
    };
    for (i, e) in doc.run_errors.iter().enumerate() {
        synthetic(&format!("run-error-{i}"), "run-error", e, &mut errors);
    }
    for (case, status) in &doc.unknown_statuses {
        synthetic(
            case,
            "unknown-status",
            &format!("unknown status `{status}`"),
            &mut errors,
        );
    }
    if !doc.terminated {
        synthetic(
            "unterminated-segment",
            "unterminated",
            "the results stream has no segment-end terminator",
            &mut errors,
        );
    }

    format!(
        "  <testsuite name=\"{}\" tests=\"{tests}\" failures=\"{failures}\" \
         errors=\"{errors}\" skipped=\"{skipped}\" time=\"{:.3}\">\n{cases}  </testsuite>\n",
        attr(target),
        total_ms as f64 / 1000.0
    )
}

/// classname/name: the case's prefix and leaf. Single-segment names
/// keep the whole name in both (JUnit consumers require non-empty
/// classnames for grouping).
fn split_case(case: &str) -> (&str, &str) {
    match case.rsplit_once('/') {
        Some((prefix, leaf)) => (prefix, leaf),
        None => (case, case),
    }
}

fn attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use component_test_formats::results::fold_jsonl;

    fn doc(jsonl: &str) -> Document {
        fold_jsonl(jsonl, &component_test_formats::selected_names(None, jsonl)).unwrap()
    }

    const STREAM: &str = r#"{"component-test-results":"0.1","target":"native","suite":{"name":"s"},"run":{"segment":0}}
{"case":"g/tc1","status":"pass","provenance":"returned","duration-ms":12,"diagnostics-complete":true}
{"case":"g/tc2","status":"fail","provenance":"trap","detail":"boom & <bust>","diagnostics":["a<b"],"diagnostics-complete":false}
{"case":"g/tc3","status":"skipped","provenance":"returned","detail":"no rng","diagnostics-complete":true}
{"case":"lonely","status":"not-applicable","detail":"hsm","diagnostics-complete":true}
{"segment-end":true}
"#;

    #[test]
    fn maps_the_status_vocabulary() {
        let xml = junit(&[("native".into(), doc(STREAM))]);
        assert!(xml.contains(
            "<testsuite name=\"native\" tests=\"4\" failures=\"1\" errors=\"0\" skipped=\"2\""
        ));
        assert!(xml.contains("<testcase classname=\"g\" name=\"tc1\" time=\"0.012\"/>"));
        assert!(xml.contains("<failure message=\"boom &amp; &lt;bust&gt;\" type=\"trap\"/>"));
        assert!(xml.contains("<system-out>a&lt;b\n[diagnostics incomplete]</system-out>"));
        assert!(xml.contains("<skipped message=\"no rng\"/>"));
        // Single-segment names group under themselves.
        assert!(xml.contains("<testcase classname=\"lonely\" name=\"lonely\""));
        assert!(xml.contains("<skipped message=\"not applicable (hsm)\"/>"));
    }

    #[test]
    fn transport_problems_become_errors_not_omissions() {
        // Unterminated stream with a run error: both surface as
        // <error> testcases and count in errors=.
        let broken = r#"{"component-test-results":"0.1","target":"t","suite":{"name":"s"},"run":{"segment":0}}
{"case":"g/tc1","status":"pass","provenance":"returned","diagnostics-complete":true}
{"run-error":"instance wedged"}
"#;
        let xml = junit(&[("t".into(), doc(broken))]);
        assert!(xml.contains("errors=\"2\""), "{xml}");
        assert!(xml.contains("<error message=\"instance wedged\" type=\"run-error\"/>"));
        assert!(xml.contains("type=\"unterminated\""));
    }
}
