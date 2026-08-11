//! `run`: execute a composed wasi:cli component — the minimal embedding
//! equivalent of `wasmtime run -W component-model-async -S p3`, so
//! `component-test run` is compose-runner + wasmtime in one step with
//! no external tools.
//!
//! Scope is exactly "sufficient for the runner core": WASI p2 (the
//! suite's and provider's std imports) + WASI p3 (the runner core's
//! `wasi:cli/stdout@0.3.0` and its `wasi:cli/run@0.3.0` entry point),
//! stdio inherited, environment explicit (wasmtime run's behavior: the
//! host env is not inherited). Known divergences from `wasmtime run`,
//! irrelevant to the runner core: stdin is closed, and no argv is
//! passed. No epoch budgets, no pooling — hangs and
//! traps are the wasmtime CLI's problem too; the serious execution
//! policy lives in the host-embed runner.

use anyhow::Result;
use wasmtime::component::{Component, Linker};
use wasmtime::error::Context as _;
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p3::bindings::Command;
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Execute `composed` (exporting `wasi:cli/run@0.3.0`) and return the
/// process exit code: the guest's `run` result (0/1), or its explicit
/// exit status. Traps and instantiation failures surface as errors.
pub fn execute(composed: &[u8], env: &[(String, String)]) -> Result<i32> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;
    let component = Component::new(&engine, composed).context("loading composed component")?;

    let mut linker: Linker<Ctx> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi::p3::add_to_linker(&mut linker)?;

    let mut wasi = WasiCtxBuilder::new();
    wasi.inherit_stdout().inherit_stderr();
    for (k, v) in env {
        wasi.env(k, v);
    }
    let mut store = Store::new(
        &engine,
        Ctx {
            wasi: wasi.build(),
            table: ResourceTable::new(),
        },
    );

    let result: wasmtime::Result<Result<(), ()>> = wasmtime_wasi::runtime::in_tokio(async {
        let command = Command::instantiate_async(&mut store, &component, &linker).await?;
        store
            .run_concurrent(async move |store| command.wasi_cli_run().call_run(store).await)
            .await?
    });

    match result {
        Ok(Ok(())) => Ok(0),
        Ok(Err(())) => Ok(1),
        // An explicit guest exit unwinds as an error carrying the
        // status (both the p2 and p3 exit interfaces).
        Err(e) => match e.downcast_ref::<wasmtime_wasi::I32Exit>() {
            Some(exit) => Ok(exit.0),
            None => Err(e.context("executing composed component").into()),
        },
    }
}
