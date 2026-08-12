//! `component-test`: tooling entry point.
//!
//! Subcommands:
//!   lock            Execution-free inventory from the suite's tags
//!                   section; `--check` is the drift gate, `--leaves`
//!                   pins generated rows from a runner enumeration.
//!   fold            Fold a JSONL results stream into the document form
//!                   + summary (the acceptance gate for one stream).
//!   aggregate       Cross-target validation + markdown matrix (#30).
//!   emit            Convert results streams for foreign consumers
//!                   (JUnit XML, #11). A converter, not a gate: always
//!                   exits 0 after writing.
//!   pins            One-rev-everywhere gate for downstream pin sets
//!                   (#54); `pins bump` is the write half (#60).
//!   wizen           Wizer pre-initialization of a suite artifact (#25,
//!                   #85): snapshot after the contract's own `all()`.
//!   compose-runner  Compose a suite (or bundle) with a context
//!                   provider and the wasi:cli runner core (#13, #85);
//!                   embedded defaults, overridable.
//!   run             compose-runner + execute under an embedded
//!                   wasmtime; the exit code is the guest's verdict.

mod compose;
mod junit;
mod pins;
mod run;

use std::io::{IsTerminal, Read};

use anyhow::{bail, Context as _};
use clap::{Args, Parser, Subcommand};
use component_test_formats::{
    aggregate, inventory,
    lockfile::{Lockfile, SuiteRef},
    manifest::Manifest,
    matrix, results, sha256_hex,
};

#[derive(Parser)]
#[command(
    name = "component-test",
    version,
    about = "polymorph:test tooling: composition, execution, inventory, and aggregation"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Execution-free inventory from the suite's tags section
    Lock(LockArgs),
    /// Fold a JSONL results stream (stdin) into the document form + summary
    Fold(FoldArgs),
    /// Cross-target validation + markdown matrix
    Aggregate(AggregateArgs),
    /// Convert results streams for foreign consumers (a converter, not a gate)
    Emit(EmitArgs),
    /// One-rev-everywhere gate for downstream pin sets
    Pins(pins::PinsArgs),
    /// Pre-initialize a suite with wizer: its own `all()` runs at build
    /// time, so every fresh instance is born with the registry built
    #[command(alias = "wizer")]
    Wizen(WizenArgs),
    /// Compose a suite with a context provider and the wasi:cli runner
    /// core into a runnable component
    ComposeRunner(ComposeRunnerArgs),
    /// Compose (unless already composed) and execute under an embedded
    /// wasmtime; the exit code is the guest's verdict
    Run(RunArgs),
}

#[derive(Args)]
struct LockArgs {
    /// Suite artifact (the suite itself, not a composed bundle:
    /// composition strips the tags section)
    #[arg(value_name = "suite.wasm")]
    suite: String,
    /// Write the lockfile here (default: stdout)
    #[arg(short, long, value_name = "tests.lock")]
    output: Option<String>,
    /// Check an existing lockfile for drift instead of writing
    #[arg(long, value_name = "tests.lock")]
    check: Option<String>,
    /// A runner's --enumerate output (one case name per line): pins
    /// each generated row's leaves, making coverage exact for them
    #[arg(long, value_name = "names.txt")]
    leaves: Option<String>,
}

#[derive(Args)]
struct FoldArgs {
    /// With a lockfile, coverage is part of acceptance: every case
    /// reported exactly once, generated leaves under their prefix
    #[arg(value_name = "tests.lock")]
    lock: Option<String>,
}

#[derive(Args)]
struct AggregateArgs {
    /// The suite inventory (coverage + applicability baseline)
    #[arg(long, value_name = "tests.lock")]
    lock: String,
    /// The targets manifest (features, targets, expected failures)
    #[arg(long, value_name = "targets.toml")]
    manifest: String,
    /// A target's results stream (repeatable)
    #[arg(
        long = "results",
        value_name = "target=path.jsonl",
        value_parser = parse_target_results
    )]
    results: Vec<(String, String)>,
    /// Write the markdown matrix here (default: stdout)
    #[arg(short, long, value_name = "matrix.md")]
    output: Option<String>,
}

#[derive(Args)]
struct EmitArgs {
    /// Output format
    #[arg(value_parser = ["junit"])]
    format: String,
    /// Fold each stream against this lockfile (not-reached synthesis
    /// for dropped cases); without it, the stream speaks for itself
    #[arg(long, value_name = "tests.lock")]
    lock: Option<String>,
    /// A target's results stream (repeatable, at least one)
    #[arg(
        long = "results",
        value_name = "target=path.jsonl",
        value_parser = parse_target_results,
        required = true
    )]
    results: Vec<(String, String)>,
    /// Write here (default: stdout)
    #[arg(short, long, value_name = "results.xml")]
    output: Option<String>,
}

#[derive(Args)]
struct WizenArgs {
    /// Suite artifact to pre-initialize (plain suites — WASI +
    /// test-context imports only; SUT-importing suites drive
    /// component_test_runner::wizen::wizen_with with their own linker)
    #[arg(value_name = "suite.wasm")]
    suite: String,
    /// Where to write the pre-initialized artifact (run it everywhere
    /// downstream — runners, lockfile checks — instead of mixing
    /// artifacts)
    #[arg(short, long, value_name = "out.wasm")]
    output: String,
    /// NAME=VAL visible during initialization, baked into the snapshot
    /// (as are entropy and clocks: init must not leave the registry
    /// parameterized on any of them; repeatable)
    #[arg(long = "env", value_name = "NAME=VAL", value_parser = parse_env)]
    env: Vec<(String, String)>,
}

#[derive(Args)]
struct ComposeRunnerArgs {
    /// Suite artifact, or a bundle re-exporting tests + test-context +
    /// factory (a suite pre-composed with its own providers)
    #[arg(value_name = "suite.wasm")]
    input: String,
    /// Where to write the composed component
    #[arg(short, long, value_name = "composed.wasm")]
    output: String,
    /// Context provider component (default: the embedded reference
    /// provider; unused when the input is a bundle)
    #[arg(long, value_name = "provider.wasm")]
    provider: Option<String>,
    /// wasi:cli runner core component (default: the embedded
    /// components/runner-cli build)
    #[arg(long, value_name = "runner.wasm")]
    runner: Option<String>,
}

#[derive(Args)]
struct RunArgs {
    /// Suite, bundle, or already-composed component (anything exporting
    /// wasi:cli/run is executed as-is)
    #[arg(value_name = "suite.wasm")]
    input: String,
    /// Context provider component (default: the embedded reference
    /// provider)
    #[arg(long, value_name = "provider.wasm")]
    provider: Option<String>,
    /// wasi:cli runner core component (default: the embedded
    /// components/runner-cli build)
    #[arg(long, value_name = "runner.wasm")]
    runner: Option<String>,
    /// Emit the JSONL results wire format (sets COMPONENT_TEST_JSONL=1
    /// in the guest; pipe into `component-test fold`)
    #[arg(long)]
    jsonl: bool,
    /// NAME=VAL in the guest environment (the host environment is not
    /// inherited, matching `wasmtime run`; repeatable)
    #[arg(long = "env", value_name = "NAME=VAL", value_parser = parse_env)]
    env: Vec<(String, String)>,
}

fn parse_target_results(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(t, p)| (t.to_string(), p.to_string()))
        .ok_or_else(|| "must be target=path.jsonl".to_string())
}

fn parse_env(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| "must be NAME=VAL".to_string())
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Lock(args) => lock(&args),
        Command::Fold(args) => fold(&args),
        Command::Aggregate(args) => aggregate_cmd(&args),
        Command::Emit(args) => emit_cmd(&args),
        Command::Pins(args) => pins::pins_cmd(&args),
        Command::Wizen(args) => wizen_cmd(&args),
        Command::ComposeRunner(args) => compose_runner_cmd(&args),
        Command::Run(args) => run_cmd(&args),
    }
}

fn lock(args: &LockArgs) -> anyhow::Result<()> {
    let suite_path = &args.suite;
    let wasm = compose::read_component(suite_path)?;

    let inv = inventory::inventory(&wasm)?;
    let artifact_sha256 = sha256_hex(&wasm);
    let name = std::path::Path::new(suite_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("suite")
        .to_string();
    let mut lf = Lockfile::with_generated(
        SuiteRef {
            name,
            artifact_sha256: Some(artifact_sha256),
        },
        inv.cases,
        inv.generated,
    );
    // An enumeration input (a runner's `--enumerate` output: one full
    // case name per line) pins each generated row's leaves. Exact
    // static cases pass through; a name matching neither is inventory
    // drift, as is a generated row the enumeration never touched.
    if let Some(path) = &args.leaves {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let static_names: std::collections::BTreeSet<std::borrow::Cow<'_, str>> =
            lf.case.iter().map(|c| c.name.as_str()).collect();
        let mut assigned: Vec<Vec<String>> = vec![Vec::new(); lf.generated.len()];
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if static_names.contains(&std::borrow::Cow::Borrowed(line)) {
                continue;
            }
            let row = lf
                .generated
                .iter()
                .enumerate()
                .filter(|(_, g)| component_test_core::name::is_under(line, &g.prefix))
                .max_by_key(|(_, g)| g.prefix.len());
            match row {
                Some((i, g)) => assigned[i].push(line[g.prefix.len() + 1..].to_string()),
                None => bail!(
                    "leaves entry `{line}` matches no static case or generated prefix \
                     (inventory drift between the enumeration and the artifact?)"
                ),
            }
        }
        for (i, gen) in lf.generated.iter_mut().enumerate() {
            if assigned[i].is_empty() {
                bail!(
                    "generated `{}` has no enumerated leaves — the enumeration and the \
                     artifact disagree",
                    gen.prefix
                );
            }
            gen.cases = std::mem::take(&mut assigned[i]);
        }
    }
    lf.validate()?;
    let toml = lf.to_toml()?;

    if let Some(check) = &args.check {
        let existing = Lockfile::from_toml(
            &std::fs::read_to_string(check).with_context(|| format!("reading {check}"))?,
        )?;
        existing.validate()?;
        // Inventory equality: names + tags (artifact hash may differ),
        // plus leaf enumerations when an enumeration was provided —
        // without one, a leaf-pinned lockfile gets its static parts
        // checked and says so.
        let static_generated_ok = existing.generated.len() == lf.generated.len()
            && existing
                .generated
                .iter()
                .zip(&lf.generated)
                .all(|(a, b)| a.prefix == b.prefix && a.tags == b.tags);
        let leaves_ok = args.leaves.is_none()
            || existing
                .generated
                .iter()
                .zip(&lf.generated)
                .all(|(a, b)| a.cases == b.cases);
        if existing.case != lf.case || !static_generated_ok || !leaves_ok {
            bail!(
                "lockfile drift: `{check}` does not match the suite's inventory \
                 (regenerate with `component-test lock` and review the diff)"
            );
        }
        if args.leaves.is_none() && existing.generated.iter().any(|g| !g.cases.is_empty()) {
            println!(
                "note: {check} pins generated leaves; static check only (pass --leaves \
                 for the full comparison)"
            );
        }
        println!("ok: {} cases match {check}", lf.case.len());
        return Ok(());
    }

    match &args.output {
        Some(path) => {
            std::fs::write(path, toml)?;
            println!("wrote {} cases to {path}", lf.case.len());
        }
        None => print!("{toml}"),
    }
    Ok(())
}

fn fold(args: &FoldArgs) -> anyhow::Result<()> {
    // Reading stdin to EOF is the first thing that happens; without a
    // pipe it blocks forever, so say so.
    if std::io::stdin().is_terminal() {
        eprintln!(
            "note: reading a JSONL results stream from stdin \
             (terminal attached — did you forget to pipe?)"
        );
    }
    let mut stream = String::new();
    std::io::stdin().read_to_string(&mut stream)?;

    let lockfile = match &args.lock {
        Some(path) => {
            let lf = Lockfile::from_toml(
                &std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?,
            )?;
            lf.validate()?;
            Some(lf)
        }
        None => None,
    };
    let selected = component_test_formats::selected_names(lockfile.as_ref(), &stream);

    let doc = results::fold_jsonl(&stream, &selected)?;

    // With a lockfile in hand, coverage is part of acceptance: every
    // case reported exactly once, generated leaves under their prefix
    // (same gate `aggregate` applies). Unknown-status events are not
    // valid reports, so a renamed status surfaces here too.
    let coverage_error = lockfile.as_ref().and_then(|lf| {
        lf.check_coverage(doc.results.iter().map(|r| r.case.as_str()))
            .err()
    });

    let mut counts = std::collections::BTreeMap::new();
    for r in &doc.results {
        *counts.entry(r.status.word()).or_insert(0u32) += 1;
        let flag = match r.status {
            results::Status::Pass => continue,
            results::Status::Fail => "FAIL",
            results::Status::Skipped => "SKIP",
            results::Status::NotReached => "NOT-REACHED",
            results::Status::NotApplicable => "N/A",
            results::Status::Deselected => "DESELECTED",
            // `Status` is #[non_exhaustive]. Unknown *wire* statuses
            // never deserialize into it (fold diverts them), so this
            // arm only fires for future variants added to the enum;
            // the kebab-case schema word is the sane default flag.
            _ => r.status.word(),
        };
        match r.detail.as_deref() {
            Some(detail) => println!("{flag}: {} — {detail}", r.case),
            None => println!("{flag}: {}", r.case),
        }
        for d in &r.diagnostics {
            println!("    diag: {d}");
        }
        if !r.diagnostics_complete {
            println!("    (diagnostics truncated)");
        }
    }
    for e in &doc.run_errors {
        println!("RUN-ERROR: {e}");
    }
    for (case, status) in &doc.unknown_statuses {
        println!("UNKNOWN-STATUS: {case} -> {status}");
    }
    if let Some(e) = &coverage_error {
        println!("COVERAGE: {e:#}");
    }
    println!(
        "\n{} results ({}terminated): {}",
        doc.results.len(),
        if doc.terminated { "" } else { "NOT " },
        counts
            .iter()
            .map(|(k, v)| format!("{v} {k}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let failed = doc.results.iter().any(|r| {
        matches!(
            r.status,
            results::Status::Fail | results::Status::NotReached
        )
    }) || !doc.run_errors.is_empty()
        || !doc.unknown_statuses.is_empty()
        || coverage_error.is_some()
        || !doc.terminated;
    std::process::exit(if failed { 1 } else { 0 });
}

fn aggregate_cmd(args: &AggregateArgs) -> anyhow::Result<()> {
    let lock_path = &args.lock;
    let manifest_path = &args.manifest;

    let lf = Lockfile::from_toml(
        &std::fs::read_to_string(lock_path).with_context(|| format!("reading {lock_path}"))?,
    )?;
    let manifest = Manifest::from_toml(
        &std::fs::read_to_string(manifest_path)
            .with_context(|| format!("reading {manifest_path}"))?,
    )?;

    let mut docs = Vec::new();
    for (target, path) in &args.results {
        let stream = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let doc = results::fold_jsonl(
            &stream,
            &component_test_formats::selected_names(Some(&lf), &stream),
        )
        .with_context(|| format!("folding results for target `{target}` ({path})"))?;
        docs.push((target.clone(), doc));
    }

    let agg = aggregate::aggregate(&lf, &manifest, &docs);
    let md = matrix::render(&agg);
    match &args.output {
        Some(path) => {
            std::fs::write(path, &md).with_context(|| format!("writing {path}"))?;
        }
        None => print!("{md}"),
    }

    for w in &agg.warnings {
        eprintln!("warning: {w}");
    }
    for e in &agg.errors {
        eprintln!("error: {e}");
    }
    let mut failures = 0usize;
    let mut total = 0usize;
    for (target, results) in &agg.results {
        for r in results.values() {
            total += 1;
            if agg.result_failing(target, r) {
                failures += 1;
            }
        }
    }
    let expected_fail = agg.expected_fail_count();
    let expected = if expected_fail > 0 {
        format!(" ({expected_fail} expected-fail)")
    } else {
        String::new()
    };
    println!(
        "{} targets, {total} results, {failures} failing{expected}, {} validation error(s){}",
        agg.targets.len(),
        agg.errors.len(),
        args.output
            .as_deref()
            .map(|p| format!("; wrote {p}"))
            .unwrap_or_default()
    );
    std::process::exit(if agg.ok() { 0 } else { 1 });
}

/// `emit <format>`: convert results streams for foreign consumers.
/// Formats: `junit`. With `--lock`, each stream folds against the
/// lockfile (not-reached synthesis for dropped cases); without, the
/// stream speaks for itself. A converter, not a gate — failures land
/// *in* the output where the consuming UI wants them, and the exit
/// code only reflects I/O; `fold`/`aggregate` remain the verdict
/// authorities.
fn emit_cmd(args: &EmitArgs) -> anyhow::Result<()> {
    let lockfile = match &args.lock {
        Some(p) => Some(Lockfile::from_toml(
            &std::fs::read_to_string(p).with_context(|| format!("reading {p}"))?,
        )?),
        None => None,
    };
    let mut docs = Vec::new();
    for (target, path) in &args.results {
        let stream = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let doc = results::fold_jsonl(
            &stream,
            &component_test_formats::selected_names(lockfile.as_ref(), &stream),
        )
        .with_context(|| format!("folding results for target `{target}` ({path})"))?;
        docs.push((target.clone(), doc));
    }
    let xml = junit::junit(&docs);
    match &args.output {
        Some(path) => std::fs::write(path, &xml).with_context(|| format!("writing {path}"))?,
        None => print!("{xml}"),
    }
    Ok(())
}

/// `wizen`: pre-initialize a plain suite (#25/#85). The library entry
/// points live in component-test-runner (`wizen`/`wizen_with`) so
/// SUT-importing embedders reuse their own linker setup; this is the
/// convenience driver for suites with no imports beyond WASI +
/// test-context.
fn wizen_cmd(args: &WizenArgs) -> anyhow::Result<()> {
    let wasm = compose::read_component(&args.suite)?;
    let wizened = component_test_runner::wizen::wizen(&wasm, &args.env)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("wizening {}", args.suite))?;
    std::fs::write(&args.output, &wizened).with_context(|| format!("writing {}", args.output))?;
    println!(
        "wizened: {} -> {} bytes ({})",
        wasm.len(),
        wizened.len(),
        args.output
    );
    Ok(())
}

fn compose_runner_cmd(args: &ComposeRunnerArgs) -> anyhow::Result<()> {
    let input = compose::read_component(&args.input)?;
    let composed = compose_from_args(&input, args.provider.as_deref(), args.runner.as_deref())?;
    std::fs::write(&args.output, &composed).with_context(|| format!("writing {}", args.output))?;
    println!(
        "wrote composed component to {} ({} bytes)",
        args.output,
        composed.len()
    );
    Ok(())
}

fn run_cmd(args: &RunArgs) -> anyhow::Result<()> {
    let input = compose::read_component(&args.input)?;
    let composed = match compose::classify(&input)? {
        compose::Input::Composed => input,
        _ => compose_from_args(&input, args.provider.as_deref(), args.runner.as_deref())?,
    };
    let mut env = args.env.clone();
    if args.jsonl {
        env.push(("COMPONENT_TEST_JSONL".into(), "1".into()));
    }
    let code = run::execute(&composed, &env)?;
    std::process::exit(code);
}

/// Shared compose step: resolve the provider/runner overrides (embedded
/// defaults otherwise) and compose.
fn compose_from_args(
    input: &[u8],
    provider: Option<&str>,
    runner: Option<&str>,
) -> anyhow::Result<Vec<u8>> {
    let embedded = |bytes: Option<&'static [u8]>, flag: &str| {
        bytes.map(<[u8]>::to_vec).with_context(|| {
            format!(
                "this component-test build carries no embedded components \
                 (built with --no-default-features); pass {flag}"
            )
        })
    };
    let provider = match provider {
        Some(path) => compose::read_component(path)?,
        None => embedded(compose::embedded_provider(), "--provider")?,
    };
    let runner = match runner {
        Some(path) => compose::read_component(path)?,
        None => embedded(compose::embedded_runner(), "--runner")?,
    };
    compose::compose(input, &provider, &runner)
}
