//! Integration tests driving the `ct-runner` binary against the built
//! fixture suite. All `#[ignore]`d: they need
//! `target/wasm32-wasip2/release/*.wasm` from `just build`, and run via
//! `just test-wasm` (part of `just all` / CI's verify job).
//!
//! These pin the behaviors the golden-output diffs can't see from the
//! sample path alone: trap containment across instances, census-order
//! emission under `--jobs`, session reuse under `--cases-per-instance`,
//! applicability flips, the envelope's artifact binding, and the
//! drift/malformed-section hard errors.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn fixture_wasm() -> PathBuf {
    suite_artifact("fixture_suite")
}

fn suite_artifact(name: &str) -> PathBuf {
    let path = workspace_root().join(format!("target/wasm32-wasip2/release/{name}.wasm"));
    assert!(
        path.exists(),
        "missing {} — run `just build` first (this test is wasm-gated)",
        path.display()
    );
    path
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn ct_runner(args: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_ct-runner"))
        .args(args)
        .output()
        .expect("spawn ct-runner");
    Run {
        code: out.status.code().expect("exit code"),
        stdout: String::from_utf8(out.stdout).unwrap(),
        stderr: String::from_utf8(out.stderr).unwrap(),
    }
}

/// Parse a JSONL run: (envelope, census-ordered case events, saw-terminator).
fn parse_jsonl(stdout: &str) -> (Value, Vec<Value>, bool) {
    let mut lines = stdout.lines().filter(|l| !l.trim().is_empty());
    let envelope: Value = serde_json::from_str(lines.next().expect("envelope")).unwrap();
    let mut cases = Vec::new();
    let mut terminated = false;
    for line in lines {
        let v: Value = serde_json::from_str(line).unwrap();
        if v.get("segment-end").is_some() {
            terminated = true;
            continue;
        }
        assert!(!terminated, "events after terminator");
        cases.push(v);
    }
    (envelope, cases, terminated)
}

fn status_of<'a>(cases: &'a [Value], name: &str) -> &'a Value {
    cases
        .iter()
        .find(|c| c["case"] == name)
        .unwrap_or_else(|| panic!("no event for {name}"))
}

#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn fixture_default_run_pins_per_case_semantics() {
    let wasm = fixture_wasm();
    let run = ct_runner(&[wasm.to_str().unwrap(), "--jsonl"]);
    assert_eq!(run.code, 1, "stderr: {}", run.stderr);

    let (envelope, cases, terminated) = parse_jsonl(&run.stdout);
    assert!(terminated);

    // Envelope: artifact binding present and correct.
    assert_eq!(envelope["component-test-results"], "0.1");
    assert_eq!(envelope["suite"]["name"], "fixture_suite");
    let bytes = std::fs::read(&wasm).unwrap();
    assert_eq!(
        envelope["suite"]["artifact-sha256"],
        component_test_formats::sha256_hex(&bytes).as_str()
    );

    // Exactly the census, in census order.
    let names: Vec<&str> = cases.iter().map(|c| c["case"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "fixture/trap/before",
            "fixture/trap/boom",
            "fixture/trap/after",
            "fixture/hsm/attest",
            "fixture/hsm/declined",
            "fixture/nested/deep/leaf",
            "fixture/gen/tc1",
            "fixture/gen/tc2",
        ]
    );

    // Trap: attributed, one-line detail, diagnostics incomplete.
    let boom = status_of(&cases, "fixture/trap/boom");
    assert_eq!(boom["status"], "fail");
    assert_eq!(boom["provenance"], "trap");
    assert_eq!(
        boom["detail"],
        "wasm trap: wasm `unreachable` instruction executed"
    );
    assert_eq!(boom["diagnostics-complete"], false);
    assert_eq!(boom["diagnostics"][0], "about to trap");

    // Poisoning containment: the very next case runs in a fresh
    // instance and passes.
    let after = status_of(&cases, "fixture/trap/after");
    assert_eq!(after["status"], "pass");
    assert_eq!(after["diagnostics"][0], "still alive in a fresh instance");

    // No missing features: the positive hsm case executes, the decline
    // probe is not applicable.
    assert_eq!(status_of(&cases, "fixture/hsm/attest")["status"], "pass");
    let declined = status_of(&cases, "fixture/hsm/declined");
    assert_eq!(declined["status"], "not-applicable");
    assert_eq!(declined["detail"], "!hsm");
}

#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn missing_feature_flips_applicability() {
    let wasm = fixture_wasm();
    let run = ct_runner(&[wasm.to_str().unwrap(), "--jsonl", "--missing", "hsm"]);
    assert_eq!(run.code, 1, "stderr: {}", run.stderr);
    let (_, cases, _) = parse_jsonl(&run.stdout);

    let attest = status_of(&cases, "fixture/hsm/attest");
    assert_eq!(attest["status"], "not-applicable");
    assert_eq!(attest["detail"], "hsm");
    assert_eq!(status_of(&cases, "fixture/hsm/declined")["status"], "pass");
}

#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn suite_artifact_flag_rebinds_the_envelope() {
    // Composed-run identity: --suite-artifact binds the envelope's suite
    // name and sha256 to the given artifact (what the suite's lockfile
    // records) instead of the executed one. Standing in for a composed
    // bundle, the fixture suite runs while claiming the sample suite's
    // identity — the mechanism under test, not a sensible pairing.
    let executed = fixture_wasm();
    let claimed = suite_artifact("sample_suite");
    let run = ct_runner(&[
        executed.to_str().unwrap(),
        "--jsonl",
        "--suite-artifact",
        claimed.to_str().unwrap(),
    ]);
    assert_eq!(run.code, 1, "stderr: {}", run.stderr);
    let (envelope, _, _) = parse_jsonl(&run.stdout);
    assert_eq!(envelope["suite"]["name"], "sample_suite");
    let bytes = std::fs::read(&claimed).unwrap();
    assert_eq!(
        envelope["suite"]["artifact-sha256"],
        component_test_formats::sha256_hex(&bytes).as_str()
    );

    // A missing path is a startup error, not a silent fallback to the
    // executed artifact's identity.
    let run = ct_runner(&[
        executed.to_str().unwrap(),
        "--jsonl",
        "--suite-artifact",
        "/nonexistent/suite.wasm",
    ]);
    assert_eq!(run.code, 2, "stderr: {}", run.stderr);
    assert!(run.stderr.contains("reading suite artifact"));
}

#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn parallel_jobs_emit_census_order() {
    let wasm = fixture_wasm();
    let sequential = ct_runner(&[wasm.to_str().unwrap(), "--jsonl"]);
    let parallel = ct_runner(&[wasm.to_str().unwrap(), "--jsonl", "--jobs", "3"]);
    assert_eq!(sequential.code, 1);
    assert_eq!(parallel.code, 1);
    // Striped workers, census-ordered emission, no timing fields:
    // byte-identical output.
    assert_eq!(sequential.stdout, parallel.stdout);
}

#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn unbounded_instance_reuse_still_contains_traps() {
    let wasm = fixture_wasm();
    let baseline = ct_runner(&[wasm.to_str().unwrap(), "--jsonl"]);
    let reused = ct_runner(&[
        wasm.to_str().unwrap(),
        "--jsonl",
        "--cases-per-instance",
        "0",
    ]);
    assert_eq!(reused.code, 1, "stderr: {}", reused.stderr);
    let (_, cases, terminated) = parse_jsonl(&reused.stdout);
    assert!(terminated);

    // Same verdicts as instance-per-case: the poisoned session is
    // recycled after the trap even in unbounded-reuse mode.
    let statuses = |out: &str| -> BTreeMap<String, String> {
        let (_, cases, _) = parse_jsonl(out);
        cases
            .iter()
            .map(|c| {
                (
                    c["case"].as_str().unwrap().to_string(),
                    c["status"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    assert_eq!(statuses(&baseline.stdout), statuses(&reused.stdout));
    assert_eq!(status_of(&cases, "fixture/trap/after")["status"], "pass");
}

/// Append one `component-test:tags@0.1` custom section to a component.
/// Payload sizes stay under 128 so single-byte LEBs suffice.
fn append_tags_section(wasm: &[u8], record: &str) -> Vec<u8> {
    const SECTION_NAME: &str = "component-test:tags@0.1";
    let mut payload = vec![SECTION_NAME.len() as u8];
    payload.extend_from_slice(SECTION_NAME.as_bytes());
    payload.extend_from_slice(record.as_bytes());
    assert!(payload.len() < 0x80, "single-byte LEB only");
    let mut out = wasm.to_vec();
    out.push(0x00); // custom section id
    out.push(payload.len() as u8);
    out.extend_from_slice(&payload);
    out
}

#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn drift_between_inventory_and_enumeration_is_fatal() {
    // A grammatically valid record naming a case `all()` doesn't have:
    // the inventory cross-check must refuse to run.
    let bytes = std::fs::read(fixture_wasm()).unwrap();
    let drifted = append_tags_section(&bytes, "zzz/phantom\n");
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("drift.wasm");
    std::fs::write(&path, drifted).unwrap();

    let run = ct_runner(&[path.to_str().unwrap()]);
    assert_eq!(run.code, 2, "stdout: {}", run.stdout);
    assert!(run.stderr.contains("inventory drift"), "{}", run.stderr);
    assert!(run.stderr.contains("zzz/phantom"), "{}", run.stderr);
}

#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn malformed_tags_section_is_fatal_not_ignored() {
    // Regression for the `.ok()` conflation: a present-but-corrupt
    // section must fail the run, not silently disable scheduling.
    let bytes = std::fs::read(fixture_wasm()).unwrap();
    let corrupt = append_tags_section(&bytes, "zzz/extra Bad_Tag\n");
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("corrupt.wasm");
    std::fs::write(&path, corrupt).unwrap();

    let run = ct_runner(&[path.to_str().unwrap(), "--missing", "hsm"]);
    assert_eq!(run.code, 2, "stdout: {}", run.stdout);
    assert!(
        run.stderr.contains("reading tags inventory"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("Bad_Tag"), "{}", run.stderr);
}

/// The `all()`-only drift direction: a `#[case_row]` fn registering a
/// name outside its prefix record (raw `Registry` access). The other
/// direction (section-only) is covered above by section appending.
#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn raw_registration_trips_drift_cross_check() {
    let wasm = suite_artifact("drift_fixture");
    let run = ct_runner(&[wasm.to_str().unwrap()]);
    assert_eq!(run.code, 2, "stdout: {}", run.stdout);
    assert!(run.stderr.contains("inventory drift"), "{}", run.stderr);
    assert!(
        run.stderr.contains("all()-only") && run.stderr.contains("driftfix/rogue-unrecorded"),
        "{}",
        run.stderr
    );
}

/// A `!feature` prefix record whose generator materializes zero rows
/// satisfies the static decline-pair lint but provides no decline
/// coverage: the runner's materialized check must refuse the run.
#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn zero_row_generator_cannot_satisfy_decline_pair() {
    let wasm = suite_artifact("zero_gen_fixture");
    let run = ct_runner(&[wasm.to_str().unwrap()]);
    assert_eq!(run.code, 2, "stdout: {}", run.stdout);
    assert!(run.stderr.contains("decline-pair check"), "{}", run.stderr);
    assert!(
        run.stderr.contains("phantom") && run.stderr.contains("zero-row generator"),
        "{}",
        run.stderr
    );
}

/// A `--only` filter matching nothing is an empty selection, not a
/// vacuous green run.
#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn only_matching_nothing_is_a_run_error() {
    let run = ct_runner(&[fixture_wasm().to_str().unwrap(), "--only", "zzz-no-match"]);
    assert_eq!(run.code, 2, "stdout: {}", run.stdout);
    assert!(run.stderr.contains("matches no cases"), "{}", run.stderr);
}
