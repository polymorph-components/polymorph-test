//! `component-test`: tooling entry point.
//!
//! Subcommands (v0):
//!   lock <suite.wasm> [-o tests.lock] [--check existing.lock]
//!       Execution-free inventory from the suite's tags section.
//!   fold [tests.lock] < results.jsonl
//!       Fold a JSONL results stream into the document form + summary.
//!   aggregate --lock tests.lock --manifest targets.toml
//!             [--results target=path.jsonl]... [-o matrix.md]
//!       Cross-target validation + markdown matrix (#30).

use std::io::Read;

use anyhow::{bail, Context as _};
use component_test_formats::{
    aggregate, inventory,
    lockfile::{Lockfile, SuiteRef},
    manifest::Manifest,
    matrix, results, sha256_hex,
};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("lock") => lock(&args[1..]),
        Some("fold") => fold(&args[1..]),
        Some("aggregate") => aggregate_cmd(&args[1..]),
        _ => {
            eprintln!(
                "usage: component-test lock <suite.wasm> [-o tests.lock] [--check tests.lock]"
            );
            eprintln!("       component-test fold [tests.lock] < results.jsonl");
            eprintln!(
                "       component-test aggregate --lock tests.lock --manifest targets.toml \
                 [--results target=path.jsonl]... [-o matrix.md]"
            );
            std::process::exit(2);
        }
    }
}

fn lock(args: &[String]) -> anyhow::Result<()> {
    let mut suite_path = None;
    let mut out_path = None;
    let mut check_path = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-o" => out_path = Some(it.next().context("-o needs a path")?.clone()),
            "--check" => check_path = Some(it.next().context("--check needs a path")?.clone()),
            _ if suite_path.is_none() => suite_path = Some(arg.clone()),
            other => bail!("unexpected argument `{other}`"),
        }
    }
    let suite_path = suite_path.context("missing suite.wasm path")?;
    let wasm = std::fs::read(&suite_path).with_context(|| format!("reading {suite_path}"))?;

    let inv = inventory::inventory(&wasm)?;
    let artifact_sha256 = sha256_hex(&wasm);
    let name = std::path::Path::new(&suite_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("suite")
        .to_string();
    let lf = Lockfile::with_generated(
        SuiteRef {
            name,
            artifact_sha256: Some(artifact_sha256),
        },
        inv.cases,
        inv.generated,
    );
    lf.validate()?;
    let toml = lf.to_toml()?;

    if let Some(check) = check_path {
        let existing = Lockfile::from_toml(
            &std::fs::read_to_string(&check).with_context(|| format!("reading {check}"))?,
        )?;
        existing.validate()?;
        // Inventory equality: names + tags (artifact hash may differ).
        if existing.case != lf.case || existing.generated != lf.generated {
            bail!(
                "lockfile drift: `{check}` does not match the suite's inventory \
                 (regenerate with `component-test lock` and review the diff)"
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
    let selected: Vec<String> = match &lockfile {
        Some(lf) => lf
            .case
            .iter()
            .map(|c| c.name.as_str().to_string())
            .collect(),
        None => stream
            .lines()
            .skip(1)
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("case").and_then(|c| c.as_str()).map(String::from))
            .collect(),
    };

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
            other => bail!("unexpected argument `{other}`"),
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

    let selected: Vec<String> = lf
        .case
        .iter()
        .map(|c| c.name.as_str().to_string())
        .collect();
    let mut docs = Vec::new();
    for (target, path) in &result_args {
        let stream = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let doc = results::fold_jsonl(&stream, &selected)
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
    for results in agg.results.values() {
        for r in results.values() {
            total += 1;
            if matches!(
                r.status,
                results::Status::Fail | results::Status::NotReached
            ) {
                failures += 1;
            }
        }
    }
    println!(
        "{} targets, {total} results, {failures} failing, {} validation error(s){}",
        agg.targets.len(),
        agg.errors.len(),
        out_path
            .as_deref()
            .map(|p| format!("; wrote {p}"))
            .unwrap_or_default()
    );
    std::process::exit(if agg.ok() { 0 } else { 1 });
}
