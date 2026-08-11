//! Component-level wizer pre-initialization driver (`wizer-preinit`,
//! feature `wizer`): snapshots a suite component after forcing its
//! registry build, so every fresh instance is born with the case table
//! built and `all()` costs only mint+lift (#25).
//!
//! Exists because the `wasmtime wizer` CLI cannot do this today
//! (findings.md #22): its invoke grammar rejects versioned interface
//! qualifiers (`polymorph:test/tests@0.1.0.all` fails to parse), its
//! unknown-import stubbing cannot synthesize the `test-context`
//! resource type, and composed bundles hit "nested components with
//! modules not currently supported". Driving wasmtime-wizer as a
//! library with our own linker — WASI plus a host `context` resource
//! whose methods are never called during init — sidesteps all three.
//!
//! The input component must export a bare `wizer-initialize: func()`
//! that forces the registry build (see components/bench-suite's
//! `wizer-init` feature for the pattern: a second inline-WIT world
//! merged by wasm-component-ld; the versioned `tests` export cannot be
//! named as the init function).
//!
//!   cargo run --release -p component-test-runner --features wizer \
//!     --bin wizer-preinit -- <in.wasm> <out.wasm> [ENV=VAL ...]

use wasmtime::component::{Linker, Resource, ResourceType};
use wasmtime::error::format_err;
use wasmtime::{Config, Engine, Result, Store};
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

struct HostContext;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: wizer-preinit <in.wasm> <out.wasm> [ENV=VAL ...]";
    let input = args.next().ok_or_else(|| format_err!("{usage}"))?;
    let output = args.next().ok_or_else(|| format_err!("{usage}"))?;
    let env: Vec<(String, String)> = args
        .map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| format_err!("bad env pair `{kv}` (want NAME=VAL)\n{usage}"))
        })
        .collect::<Result<_>>()?;

    let wasm = std::fs::read(&input)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;

    let mut linker: Linker<Ctx> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    let mut ctx_instance = linker.instance("polymorph:test/test-context@0.1.0")?;
    ctx_instance.resource(
        "context",
        ResourceType::host::<HostContext>(),
        |_, _| Ok(()),
    )?;
    // Present so the component type-checks; never called during init
    // (registry builds are pure — anything else is a suite bug this
    // driver would surface as a snapshot-time diagnostic call).
    ctx_instance.func_wrap_concurrent(
        "[method]context.diagnostic",
        |_accessor, (_this, _msg): (Resource<HostContext>, String)| Box::pin(async move { Ok(()) }),
    )?;

    let mut wasi = WasiCtxBuilder::new();
    wasi.inherit_stderr();
    for (k, v) in &env {
        wasi.env(k, v);
    }
    let mut store = Store::new(
        &engine,
        Ctx {
            wasi: wasi.build(),
            table: ResourceTable::new(),
        },
    );

    let mut wizer = wasmtime_wizer::Wizer::new();
    // The default strip of the init function leaves a dangling
    // core-instance export reference in the rewritten component
    // (invalid per wasm-tools validate); keep it (findings.md #22).
    wizer.keep_init_func(true);
    let (wizened, _rets) = wasmtime_wasi::runtime::in_tokio(wizer.run_component(
        &mut store,
        &wasm,
        async |store: &mut Store<Ctx>, component| linker.instantiate_async(store, component).await,
    ))?;

    std::fs::write(&output, &wizened)?;
    eprintln!(
        "wizened: {} -> {} bytes ({output})",
        wasm.len(),
        wizened.len(),
    );
    Ok(())
}
