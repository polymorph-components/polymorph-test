//! Integration tests for the `component-test` binary: argument
//! handling, fold acceptance semantics, and the aggregate pipeline.
//! Everything here is pure stdin/tempfile work except the `lock` tests
//! against real suite artifacts, which are `#[ignore]`d and run by
//! `just test-wasm` (after `just build`).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_component-test"))
}

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str], stdin: Option<&str>) -> Output {
    let mut cmd = bin();
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn component-test");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    Output {
        code: out.status.code().expect("exit code"),
        stdout: String::from_utf8(out.stdout).unwrap(),
        stderr: String::from_utf8(out.stderr).unwrap(),
    }
}

fn tmpfile(name: &str, contents: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

const LOCK_XY: &str = r#"
version = "0.1"
[suite]
name = "t"
[[case]]
name = "a/x"
[[case]]
name = "a/y"
"#;

const ENVELOPE: &str =
    r#"{"component-test-results":"0.1","target":"t","suite":{"name":"t"},"run":{"segment":0}}"#;

fn stream(events: &[&str], terminated: bool) -> String {
    let mut s = String::from(ENVELOPE);
    s.push('\n');
    for e in events {
        s.push_str(e);
        s.push('\n');
    }
    if terminated {
        s.push_str(r#"{"segment-end":true}"#);
        s.push('\n');
    }
    s
}

fn fold_with(lock: &Path, jsonl: &str) -> Output {
    run(&["fold", lock.to_str().unwrap()], Some(jsonl))
}

// ------------------------------------------------------- args / usage

#[test]
fn usage_and_help() {
    let out = run(&[], None);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("usage:"), "{}", out.stderr);

    for args in [
        &["--help"][..],
        &["lock", "--help"],
        &["fold", "--help"],
        &["aggregate", "--help"],
    ] {
        let out = run(args, None);
        assert_eq!(out.code, 0, "{args:?}");
        assert!(out.stdout.contains("usage:"), "{args:?}");
    }

    let out = run(&["frobnicate"], None);
    assert_eq!(out.code, 2);

    let out = run(&["lock", "--bogus"], None);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("unknown flag `--bogus`"),
        "{}",
        out.stderr
    );
}

#[test]
fn lock_rejects_non_wasm() {
    let path = tmpfile("not-wasm.txt", "hello, wasm-shaped world\n");
    let out = run(&["lock", path.to_str().unwrap()], None);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("not a WebAssembly binary"),
        "{}",
        out.stderr
    );
}

// ------------------------------------------------------ fold semantics

#[test]
fn fold_clean_run_exits_zero() {
    let lock = tmpfile("clean.lock", LOCK_XY);
    let jsonl = stream(
        &[
            r#"{"case":"a/x","status":"pass"}"#,
            r#"{"case":"a/y","status":"pass"}"#,
        ],
        true,
    );
    let out = fold_with(&lock, &jsonl);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stdout.contains("2 results (terminated): 2 pass"));
}

#[test]
fn fold_missing_case_fails_coverage() {
    let lock = tmpfile("missing.lock", LOCK_XY);
    let jsonl = stream(&[r#"{"case":"a/x","status":"pass"}"#], true);
    let out = fold_with(&lock, &jsonl);
    assert_eq!(out.code, 1);
    assert!(out.stdout.contains("COVERAGE:"), "{}", out.stdout);
    assert!(out.stdout.contains("a/y"), "{}", out.stdout);
}

#[test]
fn fold_duplicate_case_fails_coverage() {
    let lock = tmpfile("dupe.lock", LOCK_XY);
    let jsonl = stream(
        &[
            r#"{"case":"a/x","status":"pass"}"#,
            r#"{"case":"a/x","status":"pass"}"#,
            r#"{"case":"a/y","status":"pass"}"#,
        ],
        true,
    );
    let out = fold_with(&lock, &jsonl);
    assert_eq!(out.code, 1);
    assert!(out.stdout.contains("more than once"), "{}", out.stdout);
}

#[test]
fn fold_unknown_status_fails() {
    let lock = tmpfile("unknown.lock", LOCK_XY);
    let jsonl = stream(
        &[
            r#"{"case":"a/x","status":"wat"}"#,
            r#"{"case":"a/y","status":"pass"}"#,
        ],
        true,
    );
    let out = fold_with(&lock, &jsonl);
    assert_eq!(out.code, 1);
    assert!(
        out.stdout.contains("UNKNOWN-STATUS: a/x -> wat"),
        "{}",
        out.stdout
    );
}

#[test]
fn fold_explicit_not_reached_fails() {
    let lock = tmpfile("nr.lock", LOCK_XY);
    let jsonl = stream(
        &[
            r#"{"case":"a/x","status":"pass"}"#,
            r#"{"case":"a/y","status":"not-reached"}"#,
        ],
        true,
    );
    let out = fold_with(&lock, &jsonl);
    assert_eq!(out.code, 1);
    assert!(out.stdout.contains("NOT-REACHED: a/y"), "{}", out.stdout);
}

#[test]
fn fold_unterminated_fails_and_synthesizes() {
    let lock = tmpfile("unterm.lock", LOCK_XY);
    let jsonl = stream(&[r#"{"case":"a/x","status":"pass"}"#], false);
    let out = fold_with(&lock, &jsonl);
    assert_eq!(out.code, 1);
    assert!(out.stdout.contains("NOT terminated"), "{}", out.stdout);
    assert!(out.stdout.contains("NOT-REACHED: a/y"), "{}", out.stdout);
}

#[test]
fn fold_run_error_fails() {
    let lock = tmpfile("runerr.lock", LOCK_XY);
    let jsonl = stream(
        &[
            r#"{"case":"a/x","status":"pass"}"#,
            r#"{"case":"a/y","status":"pass"}"#,
            r#"{"run-error":"enumeration trapped"}"#,
        ],
        true,
    );
    let out = fold_with(&lock, &jsonl);
    assert_eq!(out.code, 1);
    assert!(
        out.stdout.contains("RUN-ERROR: enumeration trapped"),
        "{}",
        out.stdout
    );
}

#[test]
fn fold_unknown_version_refused() {
    let lock = tmpfile("ver.lock", LOCK_XY);
    let jsonl = stream(&[r#"{"case":"a/x","status":"pass"}"#], true).replace(
        r#""component-test-results":"0.1""#,
        r#""component-test-results":"9.9""#,
    );
    let out = fold_with(&lock, &jsonl);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr.contains("unsupported results version `9.9`"),
        "{}",
        out.stderr
    );
}

/// An all-generated suite has an empty static case list; its selection
/// is knowable only from the stream. Regression test for the spurious
/// "empty selection is a run error" (#37 pinned hole).
#[test]
fn fold_all_generated_lockfile() {
    let lock = tmpfile(
        "gen.lock",
        r#"
version = "0.1"
[suite]
name = "t"
[[generated]]
prefix = "g/rows"
"#,
    );
    let jsonl = stream(
        &[
            r#"{"case":"g/rows/tc1","status":"pass"}"#,
            r#"{"case":"g/rows/tc2","status":"pass"}"#,
        ],
        true,
    );
    let out = fold_with(&lock, &jsonl);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stdout.contains("2 results (terminated): 2 pass"));

    // Grammar still enforced under the prefix (#42).
    let jsonl = stream(
        &[r#"{"case":"g/rows/../../etc/passwd","status":"pass"}"#],
        true,
    );
    let out = fold_with(&lock, &jsonl);
    assert_eq!(out.code, 1);
    assert!(
        out.stdout.contains("not a valid case name"),
        "{}",
        out.stdout
    );
}

// -------------------------------------------------------- aggregate

const AGG_LOCK: &str = r#"
version = "0.1"
[suite]
name = "t"
[[case]]
name = "a/add"
[[case]]
name = "a/hsm/attest"
tags = ["hsm"]
[[case]]
name = "a/hsm/declined"
tags = ["!hsm"]
"#;

const AGG_MANIFEST: &str = r#"
version = "0.1"
[features.hsm]
kind = "gated"
[targets.native]
missing-features = []
[targets.sim]
missing-features = ["hsm"]
"#;

fn agg_stream(target: &str, results: &[(&str, &str, Option<&str>)]) -> String {
    let mut s = format!(
        r#"{{"component-test-results":"0.1","target":"{target}","suite":{{"name":"t"}},"run":{{"segment":0}}}}"#
    );
    s.push('\n');
    for (case, status, detail) in results {
        match detail {
            Some(d) => s.push_str(&format!(
                r#"{{"case":"{case}","status":"{status}","detail":"{d}"}}"#
            )),
            None => s.push_str(&format!(r#"{{"case":"{case}","status":"{status}"}}"#)),
        }
        s.push('\n');
    }
    s.push_str(r#"{"segment-end":true}"#);
    s.push('\n');
    s
}

fn aggregate(id: &str, native: &str, sim: &str) -> Output {
    aggregate_with_manifest(id, AGG_MANIFEST, native, sim)
}

fn aggregate_with_manifest(id: &str, manifest_toml: &str, native: &str, sim: &str) -> Output {
    let lock = tmpfile(&format!("{id}.lock"), AGG_LOCK);
    let manifest = tmpfile(&format!("{id}-targets.toml"), manifest_toml);
    let native_path = tmpfile(&format!("{id}-native.jsonl"), native);
    let sim_path = tmpfile(&format!("{id}-sim.jsonl"), sim);
    run(
        &[
            "aggregate",
            "--lock",
            lock.to_str().unwrap(),
            "--manifest",
            manifest.to_str().unwrap(),
            "--results",
            &format!("native={}", native_path.display()),
            "--results",
            &format!("sim={}", sim_path.display()),
        ],
        None,
    )
}

fn native_ok() -> String {
    agg_stream(
        "native",
        &[
            ("a/add", "pass", None),
            ("a/hsm/attest", "pass", None),
            ("a/hsm/declined", "not-applicable", Some("!hsm")),
        ],
    )
}

fn sim_ok() -> String {
    agg_stream(
        "sim",
        &[
            ("a/add", "pass", None),
            ("a/hsm/attest", "not-applicable", Some("hsm")),
            ("a/hsm/declined", "pass", None),
        ],
    )
}

#[test]
fn aggregate_clean_corpus() {
    let out = aggregate("agg-clean", &native_ok(), &sim_ok());
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stdout.contains("# Test matrix"));
    assert!(
        out.stdout
            .contains("2 targets, 6 results, 0 failing, 0 validation error(s)"),
        "{}",
        out.stdout
    );
}

#[test]
fn aggregate_applicability_drift_fails() {
    // sim declares hsm missing but reports the hsm-tagged case executed.
    let sim = agg_stream(
        "sim",
        &[
            ("a/add", "pass", None),
            ("a/hsm/attest", "pass", None),
            ("a/hsm/declined", "pass", None),
        ],
    );
    let out = aggregate("agg-drift", &native_ok(), &sim);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("not applicable"), "{}", out.stderr);
}

#[test]
fn aggregate_artifact_hashes_are_provenance() {
    // Lockfile hash differing from the envelopes' is expected-normal
    // (non-reproducible builds; #44): tolerated, no error, no warning.
    let lock = tmpfile(
        "agg-hash.lock",
        &AGG_LOCK.replace(
            "name = \"t\"\n[[case]]",
            "name = \"t\"\nartifact_sha256 = \"aa11\"\n[[case]]",
        ),
    );
    let manifest = tmpfile("agg-hash-targets.toml", AGG_MANIFEST);
    let native = native_ok().replace(
        r#""suite":{"name":"t"}"#,
        r#""suite":{"name":"t","artifact-sha256":"bb22"}"#,
    );
    let native_path = tmpfile("agg-hash-native.jsonl", &native);
    let sim = sim_ok().replace(
        r#""suite":{"name":"t"}"#,
        r#""suite":{"name":"t","artifact-sha256":"bb22"}"#,
    );
    let sim_path = tmpfile("agg-hash-sim.jsonl", &sim);
    let args_for = |native: &std::path::Path, sim: &std::path::Path| {
        [
            "aggregate".to_string(),
            "--lock".into(),
            lock.to_str().unwrap().into(),
            "--manifest".into(),
            manifest.to_str().unwrap().into(),
            "--results".into(),
            format!("native={}", native.display()),
            "--results".into(),
            format!("sim={}", sim.display()),
        ]
    };
    let args = args_for(&native_path, &sim_path);
    let out = run(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>(), None);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(!out.stderr.contains("warning"), "{}", out.stderr);

    // Targets disagreeing with EACH OTHER (same aggregation, mixed
    // builds) is the reproducibility-independent smell: warning only.
    let sim_other = sim_ok().replace(
        r#""suite":{"name":"t"}"#,
        r#""suite":{"name":"t","artifact-sha256":"cc33"}"#,
    );
    let sim_other_path = tmpfile("agg-hash-sim2.jsonl", &sim_other);
    let args = args_for(&native_path, &sim_other_path);
    let out = run(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>(), None);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("mixed builds"), "{}", out.stderr);
    assert!(
        out.stderr.contains("bb22") && out.stderr.contains("cc33"),
        "{}",
        out.stderr
    );
}

#[test]
fn aggregate_applies_applicability_for_unscheduled_streams() {
    // The composed-runner topology (#36): an execute-everything stream
    // declares `scheduling: none`; aggregate reclassifies executed
    // non-applicable cases instead of erroring, and the corpus is
    // clean despite the decline probe "running" on sim.
    let sim = agg_stream(
        "sim",
        &[
            ("a/add", "pass", None),
            ("a/hsm/attest", "fail", Some("no hsm on sim")),
            ("a/hsm/declined", "pass", None),
        ],
    )
    .replace(
        r#""run":{"segment":0}"#,
        r#""run":{"segment":0,"scheduling":"none"}"#,
    );
    let out = aggregate("agg-unscheduled", &native_ok(), &sim);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(out.stderr.contains("reclassified"), "{}", out.stderr);
    assert!(
        out.stdout
            .contains("2 targets, 6 results, 0 failing, 0 validation error(s)"),
        "{}",
        out.stdout
    );
}

#[test]
fn aggregate_missing_args() {
    let out = run(&["aggregate", "--manifest", "x.toml"], None);
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("missing --lock"), "{}", out.stderr);
}

const AGG_XFAIL_MANIFEST: &str = r#"
version = "0.1"
[features.hsm]
kind = "gated"
[targets.native]
missing-features = []
[targets.sim]
missing-features = ["hsm"]
[[targets.sim.expected-fail]]
case = "a/add"
reason = "integer add broken on the sim backend"
tracking = "https://example.test/issues/9"
"#;

/// #48 end to end: a declared known failure aggregates green with the
/// debt reported; the same declaration over a passing case is a
/// validation failure that forces cleanup.
#[test]
fn aggregate_expected_fail_roundtrip() {
    let sim_failing = agg_stream(
        "sim",
        &[
            ("a/add", "fail", Some("1 + 1 = 3")),
            ("a/hsm/attest", "not-applicable", Some("hsm")),
            ("a/hsm/declined", "pass", None),
        ],
    );
    let out = aggregate_with_manifest("agg-xfail", AGG_XFAIL_MANIFEST, &native_ok(), &sim_failing);
    assert_eq!(
        out.code, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(
        out.stdout
            .contains("2 targets, 6 results, 0 failing (1 expected-fail), 0 validation error(s)"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("## Expected failures"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("xfail"), "{}", out.stdout);

    // The case got fixed: the stale declaration must fail the run.
    let out = aggregate_with_manifest("agg-xpass", AGG_XFAIL_MANIFEST, &native_ok(), &sim_ok());
    assert_eq!(
        out.code, 1,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(
        out.stderr.contains("passed") && out.stderr.contains("`a/add`"),
        "{}",
        out.stderr
    );
    assert!(out.stdout.contains("XPASS"), "{}", out.stdout);
}

// ------------------------------------------- lock (needs built wasm)

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
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

#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn lock_check_matches_committed() {
    let wasm = suite_artifact("sample_suite");
    let lock = workspace_root().join("components/sample-suite/tests.lock");
    let out = run(
        &[
            "lock",
            wasm.to_str().unwrap(),
            "--check",
            lock.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("ok: 3 cases match"), "{}", out.stdout);
}

/// `--leaves` pins the fixture suite's generated row (`fixture/gen`
/// enumerates tc1/tc2 deterministically): generation renders one leaf
/// per line, a matching check passes, and a drifted enumeration —
/// leaves the artifact's rows never produced, or rows the enumeration
/// never touched — fails by name.
#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn lock_leaves_pins_generated_rows() {
    let wasm = suite_artifact("fixture_suite");
    let wasm = wasm.to_str().unwrap();
    let leaves = tmpfile(
        "leaves-fixture.txt",
        "fixture/hsm/attest\nfixture/gen/tc1\nfixture/gen/tc2\n",
    );
    let out_lock = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fixture-leaves.lock");
    let out = run(
        &[
            "lock",
            wasm,
            "--leaves",
            leaves.to_str().unwrap(),
            "-o",
            out_lock.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let text = std::fs::read_to_string(&out_lock).unwrap();
    assert!(
        text.contains("cases = [\n    \"tc1\",\n    \"tc2\",\n]"),
        "one leaf per line:\n{text}"
    );

    let out = run(
        &[
            "lock",
            wasm,
            "--leaves",
            leaves.to_str().unwrap(),
            "--check",
            out_lock.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    // Checking without an enumeration still passes on the static
    // parts, and says the comparison was partial.
    let out = run(&["lock", wasm, "--check", out_lock.to_str().unwrap()], None);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("static check only"), "{}", out.stdout);

    // A drifted enumeration fails the check.
    let drifted = tmpfile("leaves-drifted.txt", "fixture/gen/tc1\nfixture/gen/tc3\n");
    let out = run(
        &[
            "lock",
            wasm,
            "--leaves",
            drifted.to_str().unwrap(),
            "--check",
            out_lock.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(out.code, 1, "stdout: {}", out.stdout);
    assert!(out.stderr.contains("lockfile drift"), "{}", out.stderr);

    // An enumeration entry matching nothing is inventory drift.
    let alien = tmpfile("leaves-alien.txt", "fixture/gen/tc1\nother/suite/case\n");
    let out = run(&["lock", wasm, "--leaves", alien.to_str().unwrap()], None);
    assert_eq!(out.code, 1);
    assert!(
        out.stderr
            .contains("matches no static case or generated prefix"),
        "{}",
        out.stderr
    );
}

#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn lock_check_detects_drift() {
    // The sample suite's inventory against the fixture suite's lockfile
    // is guaranteed drift.
    let wasm = suite_artifact("sample_suite");
    let lock = workspace_root().join("components/fixture-suite/tests.lock");
    let out = run(
        &[
            "lock",
            wasm.to_str().unwrap(),
            "--check",
            lock.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(out.code, 1);
    assert!(out.stderr.contains("lockfile drift"), "{}", out.stderr);
}

#[test]
#[ignore = "needs built components: run via `just test-wasm`"]
fn lock_emits_inventory_to_stdout() {
    let wasm = suite_artifact("fixture_suite");
    let out = run(&["lock", wasm.to_str().unwrap()], None);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("fixture/trap/boom"), "{}", out.stdout);
    assert!(
        out.stdout.contains("prefix = \"fixture/gen\""),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("artifact_sha256"), "{}", out.stdout);
}
