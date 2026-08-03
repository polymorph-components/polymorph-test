//! `component-test`: tooling entry point.
//!
//! Subcommands (v0):
//!   lock <suite.wasm> [-o tests.lock] [--check existing.lock]
//!       Execution-free inventory from the suite's marks section.
//!   fold [tests.lock] < results.jsonl
//!       Fold a JSONL results stream into the document form + summary.

use std::io::Read;

use anyhow::{bail, Context as _};
use component_test_formats::{
    inventory,
    lockfile::{Lockfile, SuiteRef},
    results,
};
use sha2::Digest;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("lock") => lock(&args[1..]),
        Some("fold") => fold(&args[1..]),
        _ => {
            eprintln!(
                "usage: component-test lock <suite.wasm> [-o tests.lock] [--check tests.lock]"
            );
            eprintln!("       component-test fold [tests.lock] < results.jsonl");
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

    let cases = inventory::inventory(&wasm)?;
    let artifact_sha256 = hex(&sha2::Sha256::digest(&wasm));
    let name = std::path::Path::new(&suite_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("suite")
        .to_string();
    let lf = Lockfile::new(
        SuiteRef {
            name,
            artifact_sha256: Some(artifact_sha256),
        },
        cases,
    );
    lf.validate()?;
    let toml = lf.to_toml()?;

    if let Some(check) = check_path {
        let existing = Lockfile::from_toml(
            &std::fs::read_to_string(&check).with_context(|| format!("reading {check}"))?,
        )?;
        existing.validate()?;
        // Inventory equality: names + marks (artifact hash may differ).
        if existing.case != lf.case {
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

    let selected: Vec<String> = match args.first() {
        Some(path) => {
            let lf = Lockfile::from_toml(
                &std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?,
            )?;
            lf.validate()?;
            lf.case
                .iter()
                .map(|c| c.name.as_str().to_string())
                .collect()
        }
        None => stream
            .lines()
            .skip(1)
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("case").and_then(|c| c.as_str()).map(String::from))
            .collect(),
    };

    let doc = results::fold_jsonl(&stream, &selected)?;

    let mut counts = std::collections::BTreeMap::new();
    for r in &doc.results {
        *counts.entry(format!("{:?}", r.status)).or_insert(0u32) += 1;
        let flag = match r.status {
            results::Status::Pass => continue,
            results::Status::Fail => "FAIL",
            results::Status::Skipped => "SKIP",
            results::Status::NotReached => "NOT-REACHED",
            results::Status::NotApplicable => "N/A",
            results::Status::Deselected => "DESELECTED",
        };
        println!("{flag}: {} {}", r.case, r.detail.as_deref().unwrap_or(""));
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
    let failed = doc
        .results
        .iter()
        .any(|r| r.status == results::Status::Fail)
        || !doc.run_errors.is_empty()
        || !doc.terminated;
    std::process::exit(if failed { 1 } else { 0 });
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
