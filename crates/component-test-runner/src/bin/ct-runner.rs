//! Thin CLI over the runner library: `ct-runner <suite.wasm> [--jsonl]`.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Result};
use component_test_runner::{OutputMode, Runner};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let mut suite: Option<PathBuf> = None;
    let mut mode = OutputMode::Human;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--jsonl" => mode = OutputMode::Jsonl,
            _ if suite.is_none() => suite = Some(PathBuf::from(arg)),
            _ => bail!("unexpected argument `{arg}`\nusage: ct-runner <suite.wasm> [--jsonl]"),
        }
    }
    let Some(suite) = suite else {
        bail!("usage: ct-runner <suite.wasm> [--jsonl]");
    };
    let suite_name = suite
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "suite".into());

    let runner = Runner::new(&suite)?;
    let summary =
        wasmtime_wasi::runtime::in_tokio(runner.run_suite(&suite_name, mode))?;

    Ok(if summary.failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
