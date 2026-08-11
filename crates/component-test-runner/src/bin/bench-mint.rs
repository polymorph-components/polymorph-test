//! Synthetic handle-mint benchmark (`bench-mint`): measures the
//! per-fresh-instance cost of the `tests.all()` protocol at suite
//! scale, phase by phase, against a suite whose case count is set via
//! the `BENCH_CASES` env import (see `components/bench-suite`).
//!
//! Phases, timed per fresh store+instance:
//!   instantiate  store creation + linker instantiation
//!   all#1        first `all()`: guest registry build + mint + lift
//!   all#2        second `all()`: mint + lift only (registry cached)
//!   name[0]      one `test-case.name` boundary call (string lift)
//!   run[0]       one trivial case execution (borrowed context)
//!   drop         store teardown (frees 2N lifted handles + guest heap)
//!
//! Engine configuration mirrors the production `Runner` (pooling
//! allocator, CoW images, epoch instrumentation compiled in, untyped
//! `Val` calls), so numbers transfer to the real instance-per-case
//! path. Usage:
//!
//!   cargo run --release -p component-test-runner --bin bench-mint -- \
//!     target/wasm32-wasip2/release/bench_suite.wasm \
//!     [--cases 100,1000,10000] [--instances 20]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _, Result};
use wasmtime::component::{Component, Func, Instance, Linker, Resource, ResourceType, Val};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

const TESTS_INSTANCE: &str = "polymorph:test/tests@0.1.0";
const CONTEXT_INSTANCE: &str = "polymorph:test/test-context@0.1.0";

struct BenchCtx {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for BenchCtx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Unit host rep for `test-context.context`; diagnostics are discarded
/// (the bench's cases emit none).
struct HostContext;

#[derive(Default, Clone)]
struct Phases {
    instantiate: Duration,
    all1: Duration,
    all2: Duration,
    name0: Duration,
    run0: Duration,
    drop: Duration,
}

fn main() -> Result<()> {
    let mut suite: Option<PathBuf> = None;
    let mut cases: Vec<usize> = vec![100, 1000, 10000];
    let mut instances: usize = 20;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cases" => {
                let list = args.next().ok_or_else(|| anyhow!("--cases needs a list"))?;
                cases = list
                    .split(',')
                    .map(|s| s.parse().context("--cases: invalid count"))
                    .collect::<Result<_>>()?;
            }
            "--instances" => {
                instances = args
                    .next()
                    .ok_or_else(|| anyhow!("--instances needs a number"))?
                    .parse()
                    .context("--instances: invalid number")?;
            }
            s if s.starts_with('-') => bail!("unknown flag `{s}`"),
            _ if suite.is_none() => suite = Some(PathBuf::from(arg)),
            _ => bail!("unexpected argument `{arg}`"),
        }
    }
    let suite = suite.ok_or_else(|| {
        anyhow!("usage: bench-mint <bench_suite.wasm> [--cases N,N,...] [--instances M]")
    })?;

    // Engine config: byte-for-byte the production Runner's choices.
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    config.epoch_interruption(true);
    let mut pool = wasmtime::PoolingAllocationConfig::new();
    pool.total_memories(64)
        .total_tables(64)
        .total_core_instances(64)
        .total_component_instances(32)
        .max_memory_size(1 << 30)
        .max_component_instance_size(1 << 20);
    config.allocation_strategy(wasmtime::InstanceAllocationStrategy::Pooling(pool));
    let engine = Engine::new(&config)?;

    let wasm = std::fs::read(&suite)?;
    let t = Instant::now();
    let component = Component::new(&engine, &wasm)?;
    eprintln!("compile: {:?} ({} bytes)", t.elapsed(), wasm.len());

    let mut linker: Linker<BenchCtx> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    let mut ctx_instance = linker.instance(CONTEXT_INSTANCE)?;
    ctx_instance.resource(
        "context",
        ResourceType::host::<HostContext>(),
        |_, _| Ok(()),
    )?;
    ctx_instance.func_wrap_concurrent(
        "[method]context.diagnostic",
        |_accessor, (_this, _msg): (Resource<HostContext>, String)| Box::pin(async move { Ok(()) }),
    )?;

    println!(
        "{:>6} {:>12} {:>12} {:>12} {:>12} {:>12} {:>12} {:>10}",
        "cases", "instantiate", "all#1", "all#2", "name[0]", "run[0]", "drop", "ns/handle"
    );
    for n in cases {
        let samples =
            wasmtime_wasi::runtime::in_tokio(bench_n(&engine, &component, &linker, n, instances))?;
        report(n, &samples);
    }
    Ok(())
}

async fn bench_n(
    engine: &Engine,
    component: &Component,
    linker: &Linker<BenchCtx>,
    n: usize,
    instances: usize,
) -> Result<Vec<Phases>> {
    let warmup = 3.min(instances);
    let mut samples = Vec::with_capacity(instances);
    for i in 0..instances + warmup {
        let mut p = Phases::default();

        let t = Instant::now();
        let mut store = Store::new(
            engine,
            BenchCtx {
                wasi: WasiCtxBuilder::new()
                    .inherit_stderr()
                    .env("BENCH_CASES", n.to_string())
                    .build(),
                table: ResourceTable::new(),
            },
        );
        store.set_epoch_deadline(1); // parity: checks compiled in, never trips
        let instance = linker.instantiate_async(&mut store, component).await?;
        p.instantiate = t.elapsed();

        let funcs = TestsFuncs::new(&mut store, &instance)?;

        let t = Instant::now();
        let cases1 = funcs.all(&mut store).await?;
        p.all1 = t.elapsed();
        if cases1.len() != n {
            bail!("suite minted {} cases, expected {n}", cases1.len());
        }

        let t = Instant::now();
        let cases2 = funcs.all(&mut store).await?;
        p.all2 = t.elapsed();

        let t = Instant::now();
        let name = funcs.name(&mut store, &cases1[0]).await?;
        p.name0 = t.elapsed();
        if !name.starts_with("bench/mint/") {
            bail!("unexpected case name `{name}`");
        }

        let ctx = Resource::<HostContext>::new_own(1);
        let ctx_any = ctx.try_into_resource_any(&mut store)?;
        let t = Instant::now();
        let mut results = [Val::Bool(false)];
        funcs
            .run
            .call_async(
                &mut store,
                &[cases1[0].clone(), Val::Resource(ctx_any)],
                &mut results,
            )
            .await?;
        p.run0 = t.elapsed();
        if !matches!(&results[0], Val::Result(Ok(_))) {
            bail!("bench case did not pass: {:?}", results[0]);
        }

        drop(cases1);
        drop(cases2);
        let t = Instant::now();
        drop(store);
        p.drop = t.elapsed();

        if i >= warmup {
            samples.push(p);
        }
    }
    Ok(samples)
}

fn report(n: usize, samples: &[Phases]) {
    let med = |f: fn(&Phases) -> Duration| -> Duration {
        let mut v: Vec<_> = samples.iter().map(f).collect();
        v.sort();
        v[v.len() / 2]
    };
    let all2 = med(|p| p.all2);
    println!(
        "{:>6} {:>12?} {:>12?} {:>12?} {:>12?} {:>12?} {:>12?} {:>10.0}",
        n,
        med(|p| p.instantiate),
        med(|p| p.all1),
        all2,
        med(|p| p.name0),
        med(|p| p.run0),
        med(|p| p.drop),
        all2.as_nanos() as f64 / n as f64,
    );
}

/// The suite's `tests` export surface via the untyped `Val` API — the
/// same calls the production runner makes.
struct TestsFuncs {
    all: Func,
    name: Func,
    run: Func,
}

impl TestsFuncs {
    fn new(store: &mut Store<BenchCtx>, instance: &Instance) -> Result<Self> {
        let (_, tests) = instance
            .get_export(&mut *store, None, TESTS_INSTANCE)
            .ok_or_else(|| anyhow!("suite does not export `{TESTS_INSTANCE}`"))?;
        let lookup = |store: &mut Store<BenchCtx>, name: &str| -> Result<Func> {
            let (_, idx) = instance
                .get_export(&mut *store, Some(&tests), name)
                .ok_or_else(|| anyhow!("`{TESTS_INSTANCE}` does not export `{name}`"))?;
            instance
                .get_func(&mut *store, idx)
                .ok_or_else(|| anyhow!("`{name}` is not a function"))
        };
        Ok(Self {
            all: lookup(store, "all")?,
            name: lookup(store, "[method]test-case.name")?,
            run: lookup(store, "[method]test-case.run")?,
        })
    }

    async fn all(&self, store: &mut Store<BenchCtx>) -> Result<Vec<Val>> {
        let mut results = [Val::Bool(false)];
        self.all.call_async(&mut *store, &[], &mut results).await?;
        match results.into_iter().next().unwrap() {
            Val::List(cases) => Ok(cases),
            other => bail!("unexpected tests.all result: {other:?}"),
        }
    }

    async fn name(&self, store: &mut Store<BenchCtx>, case: &Val) -> Result<String> {
        let mut results = [Val::Bool(false)];
        self.name
            .call_async(&mut *store, std::slice::from_ref(case), &mut results)
            .await?;
        match results.into_iter().next().unwrap() {
            Val::String(s) => Ok(s),
            other => bail!("unexpected test-case.name result: {other:?}"),
        }
    }
}
