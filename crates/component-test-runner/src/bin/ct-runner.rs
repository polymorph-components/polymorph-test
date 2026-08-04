//! Thin CLI over the runner library:
//! `ct-runner <suite.wasm> [--jsonl] [--missing f1,f2,...] [--jobs N]
//! [--cases-per-instance N] [--target key] [--only substring]
//! [--enumerate]`.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context as _, Result};
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

const USAGE: &str = "usage: ct-runner <suite.wasm> [--jsonl] [--missing f1,f2,...] \
                     [--jobs N] [--cases-per-instance N] [--target key] \
                     [--only substring] [--enumerate]";

fn run() -> Result<ExitCode> {
    let mut suite: Option<PathBuf> = None;
    let mut mode = OutputMode::Human;
    let mut missing: Vec<String> = Vec::new();
    let mut cases_per_instance: usize = 1;
    let mut jobs: usize = 1;
    let mut target: String = "wasmtime/host".into();
    let mut only: Option<String> = None;
    let mut enumerate = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--missing" => {
                let list = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--missing needs a list"))?;
                missing.extend(list.split(',').filter(|s| !s.is_empty()).map(String::from));
            }
            "--only" => {
                only = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--only needs a substring"))?,
                );
            }
            "--enumerate" => enumerate = true,
            "--target" => {
                target = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--target needs a value"))?;
            }
            "--jobs" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--jobs needs a number"))?;
                jobs = v
                    .parse::<usize>()
                    .with_context(|| format!("--jobs: invalid number `{v}`"))?
                    .max(1);
            }
            "--cases-per-instance" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--cases-per-instance needs a number"))?;
                cases_per_instance = v
                    .parse()
                    .with_context(|| format!("--cases-per-instance: invalid number `{v}`"))?;
            }
            "--jsonl" => mode = OutputMode::Jsonl,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "-V" | "--version" => {
                println!("ct-runner {}", env!("CARGO_PKG_VERSION"));
                return Ok(ExitCode::SUCCESS);
            }
            s if s.starts_with('-') => bail!("unknown flag `{s}`\n{USAGE}"),
            _ if suite.is_none() => suite = Some(PathBuf::from(arg)),
            _ => bail!("unexpected argument `{arg}`\n{USAGE}"),
        }
    }
    let Some(suite) = suite else {
        bail!("{USAGE}");
    };
    let suite_name = suite
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "suite".into());

    let runner = Runner::new(&suite)?;
    if enumerate {
        let names = wasmtime_wasi::runtime::in_tokio(runner.enumerate())?;
        for name in names {
            println!("{name}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let summary = wasmtime_wasi::runtime::in_tokio(runner.run_suite_opts(
        &suite_name,
        &target,
        mode,
        &missing,
        cases_per_instance,
        jobs,
        only.as_deref(),
    ))?;

    Ok(if summary.failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
