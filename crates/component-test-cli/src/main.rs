//! `component-test`: tooling entry point.
//!
//! Subcommands (v0):
//!   lock <suite.wasm> [-o tests.lock] [--check existing.lock] [--leaves names.txt]
//!       Execution-free inventory from the suite's tags section.
//!       `--leaves` (a runner's `--enumerate` output) additionally pins
//!       each generated row's leaves, making coverage exact for them.
//!   fold [tests.lock] < results.jsonl
//!       Fold a JSONL results stream into the document form + summary.
//!   aggregate --lock tests.lock --manifest targets.toml
//!             [--results target=path.jsonl]... [-o matrix.md]
//!       Cross-target validation + markdown matrix (#30).
//!   emit junit [--lock tests.lock] [--results target=path.jsonl]...
//!             [-o results.xml]
//!       Convert results streams to JUnit XML for CI test UIs (#11).
//!       A converter, not a gate: always exits 0 after writing.

mod junit;

use std::io::{IsTerminal, Read};

use anyhow::{bail, Context as _};
use component_test_formats::{
    aggregate, inventory,
    lockfile::{Lockfile, SuiteRef},
    manifest::Manifest,
    matrix, results, sha256_hex,
};

fn usage() -> String {
    "usage: component-test lock <suite.wasm> [-o tests.lock] [--check tests.lock] [--leaves names.txt]\n       \
     component-test fold [tests.lock] < results.jsonl\n       \
     component-test aggregate --lock tests.lock --manifest targets.toml \
     [--results target=path.jsonl]... [-o matrix.md]\n       \
     component-test emit junit [--lock tests.lock] \
     [--results target=path.jsonl]... [-o results.xml]"
        .into()
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("lock") => lock(&args[1..]),
        Some("fold") => fold(&args[1..]),
        Some("aggregate") => aggregate_cmd(&args[1..]),
        Some("emit") => emit_cmd(&args[1..]),
        Some("-h" | "--help" | "help") => {
            println!("{}", usage());
            Ok(())
        }
        Some("-V" | "--version") => {
            println!("component-test {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("{}", usage());
            std::process::exit(2);
        }
    }
}

fn lock(args: &[String]) -> anyhow::Result<()> {
    let mut suite_path = None;
    let mut out_path = None;
    let mut check_path = None;
    let mut leaves_path = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-o" => out_path = Some(it.next().context("-o needs a path")?.clone()),
            "--check" => check_path = Some(it.next().context("--check needs a path")?.clone()),
            "--leaves" => leaves_path = Some(it.next().context("--leaves needs a path")?.clone()),
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            s if s.starts_with('-') => bail!("unknown flag `{s}`\n{}", usage()),
            _ if suite_path.is_none() => suite_path = Some(arg.clone()),
            other => bail!("unexpected argument `{other}`"),
        }
    }
    let suite_path = suite_path.context("missing suite.wasm path")?;
    let wasm = std::fs::read(&suite_path).with_context(|| format!("reading {suite_path}"))?;
    if !wasm.starts_with(b"\0asm") {
        bail!("{suite_path} is not a WebAssembly binary (bad magic)");
    }

    let inv = inventory::inventory(&wasm)?;
    let artifact_sha256 = sha256_hex(&wasm);
    let name = std::path::Path::new(&suite_path)
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
    if let Some(path) = &leaves_path {
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

    if let Some(check) = check_path {
        let existing = Lockfile::from_toml(
            &std::fs::read_to_string(&check).with_context(|| format!("reading {check}"))?,
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
        let leaves_ok = leaves_path.is_none()
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
        if leaves_path.is_none() && existing.generated.iter().any(|g| !g.cases.is_empty()) {
            println!(
                "note: {check} pins generated leaves; static check only (pass --leaves \
                 for the full comparison)"
            );
        }
        println!("ok: {} cases match {check}", lf.case.len());
        return Ok(());
    }

    match out_path {
        Some(path) => {
            std::fs::write(&path, toml)?;
            println!("wrote {} cases to {path}", lf.case.len());
        }
        None => print!("{toml}"),
    }
    Ok(())
}

fn fold(args: &[String]) -> anyhow::Result<()> {
    match args.first().map(|s| s.as_str()) {
        Some("-h" | "--help") => {
            println!("{}", usage());
            return Ok(());
        }
        Some(s) if s.starts_with('-') => bail!("unknown flag `{s}`\n{}", usage()),
        _ => {}
    }

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

    let lockfile = match args.first() {
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

fn aggregate_cmd(args: &[String]) -> anyhow::Result<()> {
    let mut lock_path = None;
    let mut manifest_path = None;
    let mut result_args: Vec<(String, String)> = Vec::new();
    let mut out_path = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--lock" => lock_path = Some(it.next().context("--lock needs a path")?.clone()),
            "--manifest" => {
                manifest_path = Some(it.next().context("--manifest needs a path")?.clone())
            }
            "--results" => {
                let spec = it.next().context("--results needs target=path.jsonl")?;
                let (target, path) = spec
                    .split_once('=')
                    .context("--results argument must be target=path.jsonl")?;
                result_args.push((target.to_string(), path.to_string()));
            }
            "-o" => out_path = Some(it.next().context("-o needs a path")?.clone()),
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other => bail!("unexpected argument `{other}`\n{}", usage()),
        }
    }
    let lock_path = lock_path.context("missing --lock")?;
    let manifest_path = manifest_path.context("missing --manifest")?;

    let lf = Lockfile::from_toml(
        &std::fs::read_to_string(&lock_path).with_context(|| format!("reading {lock_path}"))?,
    )?;
    let manifest = Manifest::from_toml(
        &std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {manifest_path}"))?,
    )?;

    let mut docs = Vec::new();
    for (target, path) in &result_args {
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
    match &out_path {
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
        out_path
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
fn emit_cmd(args: &[String]) -> anyhow::Result<()> {
    let format = match args.first().map(|s| s.as_str()) {
        Some("junit") => "junit",
        other => bail!(
            "emit: unknown or missing format {other:?} (supported: junit)\n{}",
            usage()
        ),
    };
    let mut lock_path = None;
    let mut result_args: Vec<(String, String)> = Vec::new();
    let mut out_path = None;
    let mut it = args[1..].iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--lock" => lock_path = Some(it.next().context("--lock needs a path")?.clone()),
            "--results" => {
                let spec = it.next().context("--results needs target=path.jsonl")?;
                let (target, path) = spec
                    .split_once('=')
                    .context("--results argument must be target=path.jsonl")?;
                result_args.push((target.to_string(), path.to_string()));
            }
            "-o" => out_path = Some(it.next().context("-o needs a path")?.clone()),
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other => bail!("unexpected argument `{other}`\n{}", usage()),
        }
    }
    if result_args.is_empty() {
        bail!("emit {format}: at least one --results target=path.jsonl required");
    }
    let lockfile = match &lock_path {
        Some(p) => Some(Lockfile::from_toml(
            &std::fs::read_to_string(p).with_context(|| format!("reading {p}"))?,
        )?),
        None => None,
    };
    let mut docs = Vec::new();
    for (target, path) in &result_args {
        let stream = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let doc = results::fold_jsonl(
            &stream,
            &component_test_formats::selected_names(lockfile.as_ref(), &stream),
        )
        .with_context(|| format!("folding results for target `{target}` ({path})"))?;
        docs.push((target.clone(), doc));
    }
    let xml = junit::junit(&docs);
    match &out_path {
        Some(path) => std::fs::write(path, &xml).with_context(|| format!("writing {path}"))?,
        None => print!("{xml}"),
    }
    Ok(())
}
