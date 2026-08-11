//! Component-level wizer pre-initialization (#25, findings.md 22–24):
//! snapshot a suite component after running its own `tests.all()`, so
//! every fresh instance is born with the case table built and `all()`
//! costs only mint+lift.
//!
//! Works on any unmodified suite artifact: the init function is the
//! contract's own `all()` — named in the version-last invoke syntax
//! `polymorph:test/tests.all@0.1.0()` (the parenthesized wave-call
//! form; the bare item-name form requires a `[] -> []` signature).
//! The handles `all()` returns are per-call state; only the guest heap
//! (the built registry) lands in the snapshot, at no measurable size
//! cost over a dedicated no-op init export (findings.md #22). Custom
//! sections survive, so tags scheduling, drift checks, and
//! `lock --check` all work unchanged on the wizened artifact.
//!
//! Exists because the `wasmtime wizer` CLI cannot wizen a *suite*:
//! the suite world imports `test-context`, whose `context` resource
//! cannot be synthesized by unknown-import stubbing ("resource
//! implementation is missing"), and composed bundles hit "nested
//! components with modules not currently supported". Driving
//! wasmtime-wizer as a library with our own linker — WASI plus a host
//! `context` resource whose methods init never calls — sidesteps both.
//!
//! Two entry points, mirroring [`Runner::new`]/[`Runner::with_data`]:
//! [`wizen`] for plain suites (WASI + `test-context` only), and
//! [`wizen_with`] for suites with SUT imports — component
//! instantiation is eager, so a SUT-importing suite fails to wizen
//! without its host module even though init never calls it; reuse the
//! same linker setup the embedding runner already has.
//!
//! Caveat the caller must own: the snapshot freezes whatever registry
//! construction observed (env, entropy, clocks — baked at wizen-time
//! values for every future instance). For contract-conforming suites
//! that is #25's determinism *feature*, but an env-parameterized
//! registry stops being parameterizable; run the wizened artifact
//! everywhere downstream instead of mixing artifacts.
//!
//! [`Runner::new`]: crate::Runner::new
//! [`Runner::with_data`]: crate::Runner::with_data

use wasmtime::component::{Linker, Resource, ResourceType};
use wasmtime::{Config, Engine, Result, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::CONTEXT_INSTANCE;

/// The init function: the contract's own enumeration, in version-last
/// invoke syntax (the parens select the wave-call path, which permits
/// results — finding #22).
const INIT_FUNC: &str = "polymorph:test/tests.all@0.1.0()";

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

/// Wizen a plain suite (WASI + `test-context` imports only), returning
/// the pre-initialized component bytes. `env` is the environment
/// visible during initialization — and therefore baked into the
/// snapshot for every future instance.
pub fn wizen(wasm: &[u8], env: &[(String, String)]) -> Result<Vec<u8>> {
    wizen_with(
        wasm,
        || {
            let mut wasi = WasiCtxBuilder::new();
            wasi.inherit_stderr();
            for (k, v) in env {
                wasi.env(k, v);
            }
            Ctx {
                wasi: wasi.build(),
                table: ResourceTable::new(),
            }
        },
        |_| Ok(()),
    )
}

/// Wizen a suite with SUT imports: `make_data` builds the store data
/// for the single init store; `configure_linker` wires the SUT's
/// `add_to_linker` (instantiation is eager, so every import must be
/// satisfiable even though init never calls the SUT). WASI and the
/// host `context` resource are provided here, exactly as
/// [`Runner::with_data`](crate::Runner::with_data) does for execution.
pub fn wizen_with<D: WasiView + Send + 'static>(
    wasm: &[u8],
    make_data: impl FnOnce() -> D,
    configure_linker: impl FnOnce(&mut Linker<D>) -> Result<()>,
) -> Result<Vec<u8>> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;

    let mut linker: Linker<D> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    configure_linker(&mut linker)?;
    let mut ctx_instance = linker.instance(CONTEXT_INSTANCE)?;
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

    let mut store = Store::new(&engine, make_data());

    let mut wizer = wasmtime_wizer::Wizer::new();
    wizer.init_func(INIT_FUNC);
    // Mandatory: "stripping" the init function would remove the
    // `tests.all` export itself. (Stripping a dedicated init export is
    // also currently broken upstream — dangling core-instance export
    // reference, bytecodealliance/wasmtime#13168.)
    wizer.keep_init_func(true);
    let (wizened, _rets) = wasmtime_wasi::runtime::in_tokio(wizer.run_component(
        &mut store,
        wasm,
        async |store: &mut Store<D>, component| linker.instantiate_async(store, component).await,
    ))?;
    Ok(wizened)
}
