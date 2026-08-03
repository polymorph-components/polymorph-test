//! Fold a component-test JSONL results stream (stdin) into the document
//! form, printing a human summary. Optional arg: lockfile path, used as
//! the selection for the `not-reached` fold rule; without it, selection
//! defaults to the reported cases (no truncation detection).
//!
//! Usage:  ... | cargo run -p component-test-formats --example fold [tests.lock]

use std::io::Read;

use component_test_formats::{lockfile::Lockfile, results};

fn main() -> anyhow::Result<()> {
    let mut stream = String::new();
    std::io::stdin().read_to_string(&mut stream)?;

    let selected: Vec<String> = match std::env::args().nth(1) {
        Some(path) => {
            let lf = Lockfile::from_toml(&std::fs::read_to_string(path)?)?;
            lf.validate()?;
            lf.case.iter().map(|c| c.name.as_str().to_string()).collect()
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
        println!(
            "{flag}: {} {}",
            r.case,
            r.detail.as_deref().unwrap_or("")
        );
        for d in &r.diagnostics {
            println!("    diag: {d}");
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
    std::process::exit(if doc.results.iter().any(|r| r.status == results::Status::Fail) {
        1
    } else {
        0
    });
}
