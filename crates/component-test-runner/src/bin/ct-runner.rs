//! Thin CLI over the runner library:
//! `ct-runner <suite.wasm> [--jsonl] [--missing f1,f2,...] [--jobs N]
//! [--cases-per-instance N] [--target key] [--only substring]
//! [--enumerate] [--suite-artifact suite.wasm]
//! [--case-execution-budget secs] [--case-timeout secs]`.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context as _, Result};
use component_test_runner::{
    OutputMode, Runner, DEFAULT_CASE_EXECUTION_BUDGET_SECS, DEFAULT_CASE_TIMEOUT_SECS,
};

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
                     [--only substring] [--enumerate] [--suite-artifact suite.wasm] \
                     [--case-execution-budget secs] [--case-timeout secs]";

fn run() -> Result<ExitCode> {
    let mut suite: Option<PathBuf> = None;
    let mut mode = OutputMode::Human;
    let mut missing: Vec<String> = Vec::new();
    let mut cases_per_instance: usize = 1;
    let mut jobs: usize = 1;
    let mut target: String = "wasmtime/host".into();
    let mut only: Option<String> = None;
    let mut enumerate = false;
    let mut suite_artifact: Option<PathBuf> = None;
    let mut case_execution_budget: u64 = DEFAULT_CASE_EXECUTION_BUDGET_SECS;
    let mut case_timeout: u64 = DEFAULT_CASE_TIMEOUT_SECS;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--missing" => {
                let list = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--missing needs a list"))?;
                missing.extend(list.split(',').filter(|s| !s.is_empty()).map(String::from));
            }
            "--suite-artifact" => {
                // For composed runs: the *suite* component the executed
                // bundle was composed from. The results envelope binds
                // that artifact (name and sha256) instead of the bundle,
                // matching what the suite's lockfile records.
                suite_artifact =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        anyhow::anyhow!("--suite-artifact needs a path")
                    })?));
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
            "--case-execution-budget" => {
                let v = args.next().ok_or_else(|| {
                    anyhow::anyhow!("--case-execution-budget needs seconds (0 disables)")
                })?;
                case_execution_budget = v
                    .parse()
                    .with_context(|| format!("--case-execution-budget: invalid number `{v}`"))?;
            }
            "--case-timeout" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--case-timeout needs seconds (0 disables)"))?;
                case_timeout = v
                    .parse()
                    .with_context(|| format!("--case-timeout: invalid number `{v}`"))?;
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
    // The envelope's suite identity: the composed-from suite artifact
    // when given, the executed artifact otherwise.
    let named = suite_artifact.as_ref().unwrap_or(&suite);
    let suite_name = named
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "suite".into());

    let mut runner = Runner::new(&suite)?;
    if let Some(path) = &suite_artifact {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading suite artifact {}", path.display()))?;
        runner.bind_suite_artifact(&bytes);
    }
    let runner = runner;
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
        case_execution_budget,
        case_timeout,
    ))?;

    Ok(if summary.failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
