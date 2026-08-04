//! Host-embedding runner for `lann:component-test` suites.
//!
//! Loads a suite component (world `suite` from `wit/tests.wit`), provides
//! its `test-context` import host-side, and runs every case in a fresh
//! instance (instance-per-case). The suite's exports are async-lifted
//! (component-model-async), so calls go through wasmtime's concurrent
//! API; host-side `diagnostic` is a concurrent host function.

use std::path::Path;

use component_test_core::{Provenance, Tags};
use component_test_formats::results::{
    CaseResult, Envelope, Event, RunInfo, Status, SuiteInfo, RESULTS_VERSION, TERMINATOR,
};
use wasmtime::component::{Component, Func, Instance, Linker, Resource, ResourceType, Val};
use wasmtime::error::{bail, format_err, Context as _};
use wasmtime::Result;
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

const TESTS_INSTANCE: &str = "lann:component-test/tests@0.1.0";
const CONTEXT_INSTANCE: &str = "lann:component-test/test-context@0.1.0";

/// The runner's own per-case state (the diagnostic sink); embed one in
/// custom store data and expose it via [`RunnerView`].
#[derive(Default)]
pub struct CtCtx {
    /// Diagnostics reported by the currently running case.
    diagnostics: Vec<String>,
    /// Print each diagnostic as it arrives (human mode).
    live_print: bool,
}

/// Store-data trait for [`Runner`]: expose the runner's state. Combined
/// with `WasiView`, this is everything the runner itself needs; embed
/// SUT contexts alongside and wire them in the linker hook of
/// [`Runner::with_data`].
pub trait RunnerView: WasiView {
    fn ct(&mut self) -> &mut CtCtx;
}

/// Store data for plain suites (no SUT imports): WASI plus the
/// diagnostic sink.
pub struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
    ct: CtCtx,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl RunnerView for Ctx {
    fn ct(&mut self) -> &mut CtCtx {
        &mut self.ct
    }
}

/// Host representation of the `test-context.context` resource. The state
/// lives in [`Ctx`]; the resource itself is a unit marker.
pub struct HostContext;

#[derive(Debug, Clone, Copy)]
pub enum OutputMode {
    Human,
    Jsonl,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub not_applicable: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl Summary {
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped + self.not_applicable
    }
}

enum Verdict {
    Pass,
    Fail(String),
    Skip(String),
    /// The `run` call trapped (or otherwise failed at the host boundary).
    Trap(String),
}

pub struct Runner<D: RunnerView + 'static = Ctx> {
    engine: Engine,
    component: Component,
    linker: Linker<D>,
    wasm_bytes: Vec<u8>,
    make_data: Box<dyn Fn() -> D + Send + Sync>,
}

impl Runner<Ctx> {
    /// Runner for plain suites (WASI + test-context only).
    pub fn new(suite_path: &Path) -> Result<Self> {
        Runner::with_data(
            suite_path,
            || Ctx {
                wasi: WasiCtxBuilder::new().inherit_stderr().build(),
                table: ResourceTable::new(),
                ct: CtCtx::default(),
            },
            |_| Ok(()),
        )
    }
}

impl<D: RunnerView + 'static> Runner<D> {
    /// Runner for suites with SUT imports: `make_data` builds the store
    /// data for each fresh instance (embed the SUT context there);
    /// `configure_linker` wires the SUT's `add_to_linker`.
    pub fn with_data(
        suite_path: &Path,
        make_data: impl Fn() -> D + Send + Sync + 'static,
        configure_linker: impl FnOnce(&mut Linker<D>) -> Result<()>,
    ) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.wasm_component_model_async(true);
        // Instance-per-case makes instantiation the hot path: reuse
        // pooled slots (and their CoW memory mappings) instead of
        // fresh mmaps per case. Sequential execution needs only a few
        // live instances; keep the pool small but roomy per-instance.
        let mut pool = wasmtime::PoolingAllocationConfig::new();
        pool.total_memories(64)
            .total_tables(64)
            .total_core_instances(64)
            .total_component_instances(32)
            .max_memory_size(1 << 30)
            .max_component_instance_size(1 << 20);
        config.allocation_strategy(wasmtime::InstanceAllocationStrategy::Pooling(pool));
        let engine = Engine::new(&config)?;

        let wasm_bytes = std::fs::read(suite_path)
            .with_context(|| format!("reading suite component {}", suite_path.display()))?;
        // Pre-check the magic so a wrong file yields "not WebAssembly"
        // instead of the WAT parser's internals on the first byte.
        if !wasm_bytes.starts_with(b"\0asm") {
            bail!(
                "{} is not a WebAssembly binary (bad magic; expected a \
                 component built with --target wasm32-wasip2)",
                suite_path.display()
            );
        }
        let component = Component::new(&engine, &wasm_bytes)
            .with_context(|| format!("loading suite component {}", suite_path.display()))?;

        let mut linker: Linker<D> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        configure_linker(&mut linker)?;

        let mut ctx_instance = linker.instance(CONTEXT_INSTANCE)?;
        ctx_instance.resource(
            "context",
            ResourceType::host::<HostContext>(),
            |_, _| Ok(()),
        )?;
        ctx_instance.func_wrap_concurrent(
            "[method]context.diagnostic",
            |accessor, (_this, msg): (Resource<HostContext>, String)| {
                Box::pin(async move {
                    accessor.with(|mut access| {
                        let ct = access.data_mut().ct();
                        if ct.live_print {
                            println!("    diag: {msg}");
                        }
                        ct.diagnostics.push(msg);
                    });
                    Ok(())
                })
            },
        )?;

        Ok(Self {
            engine,
            component,
            linker,
            wasm_bytes,
            make_data: Box::new(make_data),
        })
    }

    fn new_store(&self, live_print: bool) -> Result<Store<D>> {
        let mut store = Store::new(&self.engine, (self.make_data)());
        store.data_mut().ct().live_print = live_print;
        Ok(store)
    }

    async fn instantiate(&self, store: &mut Store<D>) -> Result<Instance> {
        self.linker
            .instantiate_async(&mut *store, &self.component)
            .await
    }

    /// Enumerate the suite in a fresh instance: case names, in suite order.
    pub async fn enumerate(&self) -> Result<Vec<String>> {
        let mut store = self.new_store(false)?;
        let instance = self.instantiate(&mut store).await?;
        let funcs = TestsFuncs::new(&mut store, &instance)?;
        let cases = funcs.all(&mut store).await?;
        let mut names = Vec::with_capacity(cases.len());
        for case in &cases {
            names.push(funcs.name(&mut store, case).await?);
        }
        Ok(names)
    }

    /// Start a session: fresh store + instance + enumeration, able to
    /// serve multiple cases (the instance-granularity knob).
    async fn new_session(&self, live_print: bool) -> Result<Session<D>> {
        let mut store = self.new_store(live_print)?;
        let instance = self.instantiate(&mut store).await?;
        let funcs = TestsFuncs::new(&mut store, &instance)?;
        let t = std::time::Instant::now();
        let cases = funcs.all(&mut store).await?;
        let first = t.elapsed();
        if std::env::var("COMPONENT_TEST_PROFILE").is_ok() {
            let t = std::time::Instant::now();
            let cases2 = funcs.all(&mut store).await?;
            let second = t.elapsed();
            eprintln!(
                "profile all#1={}us all#2={}us ({} handles; #2 = lift-only, registry cached)",
                first.as_micros(),
                second.as_micros(),
                cases2.len()
            );
        }
        Ok(Session {
            store,
            funcs,
            cases,
            served: 0,
        })
    }

    /// Run case `index` in the given session. Returns the name (from
    /// this instance) and what happened.
    async fn run_case(
        &self,
        session: &mut Session<D>,
        index: usize,
    ) -> Result<(String, Verdict, Vec<String>)> {
        session.served += 1;
        let store = &mut session.store;
        let case = session
            .cases
            .get(index)
            .cloned()
            .ok_or_else(|| format_err!("case index {index} out of range on re-enumeration"))?;
        let name = session.funcs.name(&mut *store, &case).await?;

        // Host context resource for this case, lent to the guest as a
        // borrow.
        let ctx = Resource::<HostContext>::new_own(session.served as u32);
        let ctx_any = ctx.try_into_resource_any(&mut *store)?;

        let mut results = [Val::Bool(false)];
        let call = session
            .funcs
            .run
            .call_async(&mut *store, &[case, Val::Resource(ctx_any)], &mut results)
            .await;

        let diagnostics = std::mem::take(&mut store.data_mut().ct().diagnostics);

        let verdict = match call {
            Err(e) => Verdict::Trap(trap_detail(&e)),
            Ok(()) => match &results[0] {
                Val::Result(Ok(_)) => Verdict::Pass,
                Val::Result(Err(payload)) => match payload.as_deref() {
                    Some(Val::Variant(tag, detail)) => {
                        let detail = match detail.as_deref() {
                            Some(Val::String(s)) => s.clone(),
                            _ => String::new(),
                        };
                        match tag.as_str() {
                            "failed" => Verdict::Fail(detail),
                            "skipped" => Verdict::Skip(detail),
                            other => bail!("unknown outcome variant `{other}`"),
                        }
                    }
                    other => bail!("unexpected outcome payload: {other:?}"),
                },
                other => bail!("unexpected run result: {other:?}"),
            },
        };

        Ok((name, verdict, diagnostics))
    }

    /// Run the whole suite, reporting to stdout in `mode`. Returns the
    /// summary (caller decides the exit code).
    pub async fn run_suite(
        &self,
        suite_name: &str,
        mode: OutputMode,
        missing_features: &[String],
    ) -> Result<Summary> {
        self.run_suite_with(suite_name, mode, missing_features, 1, 1)
            .await
    }

    /// `cases_per_instance`: the instance-granularity knob. 1 =
    /// instance-per-case (maximum isolation; the default); 0 =
    /// unlimited (single instance for the whole run; cheap suites);
    /// K = fresh instance every K cases. A trap always abandons the
    /// current instance regardless.
    /// `jobs`: worker parallelism. Workers share the compiled engine,
    /// own their stores, and run the modulo stripe
    /// `runnable-index % jobs == worker` (expensive cases cluster, so
    /// stripes balance better than chunks). Results are emitted in
    /// census order regardless of completion order, so output is
    /// byte-stable. With `jobs > 1`, `cases_per_instance = 0` means one
    /// instance per worker, and diagnostics print with their case's
    /// block rather than live.
    pub async fn run_suite_with(
        &self,
        suite_name: &str,
        mode: OutputMode,
        missing_features: &[String],
        cases_per_instance: usize,
        jobs: usize,
    ) -> Result<Summary> {
        self.run_suite_full(
            suite_name,
            "wasmtime/host",
            mode,
            missing_features,
            cases_per_instance,
            jobs,
        )
        .await
    }

    /// Full-options variant; `target` stamps the results envelope (the
    /// manifest key for aggregation).
    #[allow(clippy::too_many_arguments)]
    pub async fn run_suite_full(
        &self,
        suite_name: &str,
        target: &str,
        mode: OutputMode,
        missing_features: &[String],
        cases_per_instance: usize,
        jobs: usize,
    ) -> Result<Summary> {
        self.run_suite_opts(
            suite_name,
            target,
            mode,
            missing_features,
            cases_per_instance,
            jobs,
            None,
        )
        .await
    }

    /// `only`: run only cases whose name contains the substring (a
    /// dev-loop filter; filtered cases are omitted from output, so
    /// filtered runs will not aggregate cleanly — by design).
    #[allow(clippy::too_many_arguments)]
    pub async fn run_suite_opts(
        &self,
        suite_name: &str,
        target: &str,
        mode: OutputMode,
        missing_features: &[String],
        cases_per_instance: usize,
        jobs: usize,
        only: Option<&str>,
    ) -> Result<Summary> {
        let human = matches!(mode, OutputMode::Human);

        // Static inventory (tags) from the suite artifact. Absence is
        // legitimate (suite not built with the SDK); a *malformed*
        // section is a harness bug and must not silently degrade into
        // "no scheduling, no drift checks".
        let inventory = component_test_formats::inventory::try_inventory(&self.wasm_bytes)
            .map_err(|e| format_err!("reading tags inventory from suite artifact: {e:#}"))?;
        if inventory.is_none() {
            if !missing_features.is_empty() {
                bail!(
                    "--missing requires a tags inventory, but the suite artifact has no \
                     `component-test:tags@0.1` section (suite not built with the SDK, or \
                     sections stripped by composition)"
                );
            }
            eprintln!(
                "note: no tags inventory in suite artifact; \
                 feature scheduling and drift checks disabled"
            );
        }
        let tags_of = |name: &str| -> Option<Tags> {
            let inv = inventory.as_ref()?;
            if let Some(e) = inv.cases.iter().find(|e| e.name.as_str() == name) {
                return Some(Tags::new(e.tags.clone()).expect("validated by inventory parse"));
            }
            inv.generated
                .iter()
                .filter(|g| {
                    name.strip_prefix(g.prefix.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
                })
                .max_by_key(|g| g.prefix.len())
                .map(|g| Tags::new(g.tags.clone()).expect("validated by inventory parse"))
        };

        if !human {
            let envelope = Envelope {
                version: RESULTS_VERSION.into(),
                target: target.into(),
                suite: SuiteInfo {
                    name: suite_name.into(),
                    // Binds the results to the exact suite build;
                    // `aggregate` cross-checks it against the lockfile.
                    artifact_sha256: Some(component_test_formats::sha256_hex(&self.wasm_bytes)),
                    ..Default::default()
                },
                run: RunInfo::default(),
            };
            println!("{}", serde_json::to_string(&envelope)?);
        }

        let names = self.enumerate().await.context("enumerating suite")?;
        // Normative rule ("empty selection is a run error"): a suite
        // whose cases were all compiled away must not report vacuous
        // success. SDK suites can't be empty (the macro rejects it);
        // this guards non-SDK producers.
        if names.is_empty() {
            bail!("suite enumerated zero cases (empty selection is a run error)");
        }
        let mut summary = Summary::default();

        // Runtime cross-check: the static inventory and `all()` must
        // agree (drift = harness bug). Exact records match exactly;
        // enumerated names may otherwise fall under a generated-row
        // prefix.
        if let Some(inv) = &inventory {
            let enumerated: std::collections::BTreeSet<&str> =
                names.iter().map(|s| s.as_str()).collect();
            let mut missing: Vec<String> = inv
                .cases
                .iter()
                .map(|e| e.name.to_string())
                .filter(|n| !enumerated.contains(n.as_str()))
                .collect();
            let mut unrecorded: Vec<&str> = names
                .iter()
                .map(|s| s.as_str())
                .filter(|n| tags_of(n).is_none())
                .collect();
            missing.sort_unstable();
            unrecorded.sort_unstable();
            if !missing.is_empty() || !unrecorded.is_empty() {
                bail!(
                    "inventory drift: tags section and all() disagree \
                     (section-only: {missing:?}; all()-only: {unrecorded:?})",
                );
            }

            // Runtime decline-pair check over *materialized* cases: a
            // zero-row generator must not vacuously satisfy the static
            // lint (a `!feature` prefix record that produced no cases
            // provides no decline coverage).
            let mut positive = std::collections::BTreeSet::new();
            let mut negative = std::collections::BTreeSet::new();
            for name in &names {
                if let Some(tags) = tags_of(name) {
                    for tag in tags.iter() {
                        if tag.is_negative() {
                            negative.insert(tag.feature().to_string());
                        } else {
                            positive.insert(tag.feature().to_string());
                        }
                    }
                }
            }
            let unpaired: Vec<&String> = positive.difference(&negative).collect();
            if !unpaired.is_empty() {
                bail!(
                    "decline-pair check: feature(s) {unpaired:?} have materialized \
                     positively-tagged cases but no materialized `!feature` case \
                     (a zero-row generator cannot satisfy the lint)"
                );
            }
        }

        // Scheduler pre-pass: census order, each entry either runs or
        // is not-applicable (with the excluding tag).
        enum Action {
            Run,
            NotApplicable(String),
        }
        let plan: Vec<(usize, &String, Action)> = names
            .iter()
            .enumerate()
            .filter(|(_, name)| only.is_none_or(|o| name.contains(o)))
            .map(|(index, name)| {
                let action = match (&inventory, tags_of(name)) {
                    (Some(_), Some(tags)) if !tags.applies(missing_features) => {
                        Action::NotApplicable(
                            tags.excluding_mark(missing_features)
                                .map(|m| m.to_string())
                                .unwrap_or_default(),
                        )
                    }
                    _ => Action::Run,
                };
                (index, name, action)
            })
            .collect();

        // Parallel path: workers own stores; results are collected and
        // emitted in census order below.
        let mut parallel_results: std::collections::HashMap<usize, (String, Verdict, Vec<String>)> =
            std::collections::HashMap::new();
        if jobs > 1 {
            let runnable: Vec<usize> = plan
                .iter()
                .filter(|(_, _, a)| matches!(a, Action::Run))
                .map(|(i, _, _)| *i)
                .collect();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                for worker in 0..jobs {
                    let tx = tx.clone();
                    let stripe: Vec<usize> = runnable
                        .iter()
                        .enumerate()
                        .filter(|(k, _)| k % jobs == worker)
                        .map(|(_, i)| *i)
                        .collect();
                    let names = &names;
                    scope.spawn(move || {
                        wasmtime_wasi::runtime::in_tokio(async move {
                            let mut session: Option<Session<D>> = None;
                            for index in stripe {
                                if session.as_ref().is_some_and(|s| {
                                    cases_per_instance != 0 && s.served >= cases_per_instance
                                }) {
                                    session = None;
                                }
                                if session.is_none() {
                                    match self.new_session(false).await {
                                        Ok(s) => session = Some(s),
                                        Err(e) => {
                                            let _ = tx.send((
                                                index,
                                                (
                                                    names[index].clone(),
                                                    Verdict::Trap(trap_detail(&e)),
                                                    Vec::new(),
                                                ),
                                            ));
                                            continue;
                                        }
                                    }
                                }
                                let result =
                                    match self.run_case(session.as_mut().unwrap(), index).await {
                                        Ok(r) => r,
                                        Err(e) => (
                                            names[index].clone(),
                                            Verdict::Trap(trap_detail(&e)),
                                            Vec::new(),
                                        ),
                                    };
                                if matches!(result.1, Verdict::Trap(_)) {
                                    session = None;
                                }
                                let _ = tx.send((index, result));
                            }
                        });
                    });
                }
                drop(tx);
                while let Ok((index, result)) = rx.recv() {
                    parallel_results.insert(index, result);
                }
            });
        }

        let mut session: Option<Session<D>> = None;
        for (index, enumerated_name, action) in plan {
            if let Action::NotApplicable(mark) = action {
                summary.not_applicable += 1;
                if human {
                    println!("test {enumerated_name}: N/A ({mark})");
                } else {
                    let event = Event::Case(CaseResult {
                        case: enumerated_name.clone(),
                        status: Status::NotApplicable,
                        provenance: None,
                        detail: Some(mark),
                        seed: None,
                        duration_ms: None,
                        diagnostics: vec![],
                        diagnostics_complete: true,
                    });
                    println!("{}", serde_json::to_string(&event)?);
                }
                continue;
            }

            if human {
                println!("test {enumerated_name} ...");
            }

            let (name, verdict, diagnostics) = if jobs > 1 {
                let r = parallel_results
                    .remove(&index)
                    .expect("every runnable index has a worker result");
                if human {
                    for d in &r.2 {
                        println!("    diag: {d}");
                    }
                }
                r
            } else {
                if session
                    .as_ref()
                    .is_some_and(|s| cases_per_instance != 0 && s.served >= cases_per_instance)
                {
                    session = None;
                }
                if session.is_none() {
                    session = Some(self.new_session(human).await?);
                }
                let result = match self.run_case(session.as_mut().unwrap(), index).await {
                    Ok(r) => r,
                    Err(e) => (
                        enumerated_name.clone(),
                        Verdict::Trap(trap_detail(&e)),
                        Vec::new(),
                    ),
                };
                if matches!(result.1, Verdict::Trap(_)) {
                    // Poisoned: abandon the instance whatever the knob
                    // says.
                    session = None;
                }
                result
            };

            if human {
                match &verdict {
                    Verdict::Pass => println!("test {name}: PASS"),
                    Verdict::Fail(d) => println!("test {name}: FAIL: {d}"),
                    Verdict::Skip(d) => println!("test {name}: SKIP: {d}"),
                    Verdict::Trap(d) => println!("test {name}: FAIL: trap: {d}"),
                }
            } else {
                let (status, provenance, detail, complete) = match &verdict {
                    Verdict::Pass => (Status::Pass, Provenance::Returned, None, true),
                    Verdict::Fail(d) => (Status::Fail, Provenance::Returned, Some(d.clone()), true),
                    Verdict::Skip(d) => {
                        (Status::Skipped, Provenance::Returned, Some(d.clone()), true)
                    }
                    Verdict::Trap(d) => (Status::Fail, Provenance::Trap, Some(d.clone()), false),
                };
                let event = Event::Case(CaseResult {
                    case: name.clone(),
                    status,
                    provenance: Some(provenance),
                    detail,
                    seed: None,
                    duration_ms: None,
                    diagnostics,
                    diagnostics_complete: complete,
                });
                println!("{}", serde_json::to_string(&event)?);
            }

            match verdict {
                Verdict::Pass => summary.passed += 1,
                Verdict::Fail(_) | Verdict::Trap(_) => summary.failed += 1,
                Verdict::Skip(_) => summary.skipped += 1,
            }
        }

        if human {
            println!(
                "\nresult: {} passed, {} failed, {} skipped, {} not applicable, {} total",
                summary.passed,
                summary.failed,
                summary.skipped,
                summary.not_applicable,
                summary.total()
            );
        } else {
            println!("{TERMINATOR}");
        }

        Ok(summary)
    }
}

/// Reduce a wasmtime error chain to a one-line trap detail.
fn trap_detail(e: &wasmtime::Error) -> String {
    let full = format!("{e:#}");
    // Prefer the root "wasm trap: ..." message; otherwise first line.
    full.lines()
        .rev()
        .find_map(|l| l.split("wasm trap: ").nth(1))
        .map(|m| format!("wasm trap: {m}"))
        .unwrap_or_else(|| full.lines().next().unwrap_or("trap").to_string())
}

/// A live suite instance serving cases (see `run_suite_with`).
struct Session<D: RunnerView + 'static> {
    store: Store<D>,
    funcs: TestsFuncs,
    cases: Vec<Val>,
    served: usize,
}

/// The suite's `tests` export surface, looked up dynamically.
struct TestsFuncs {
    all: Func,
    name: Func,
    run: Func,
}

impl TestsFuncs {
    fn new<D: RunnerView + 'static>(store: &mut Store<D>, instance: &Instance) -> Result<Self> {
        let (_, tests) = instance
            .get_export(&mut *store, None, TESTS_INSTANCE)
            .ok_or_else(|| format_err!("suite does not export `{TESTS_INSTANCE}`"))?;
        let lookup = |store: &mut Store<D>, name: &str| -> Result<Func> {
            let (_, idx) = instance
                .get_export(&mut *store, Some(&tests), name)
                .ok_or_else(|| format_err!("`{TESTS_INSTANCE}` does not export `{name}`"))?;
            instance
                .get_func(&mut *store, idx)
                .ok_or_else(|| format_err!("`{name}` is not a function"))
        };
        Ok(Self {
            all: lookup(store, "all")?,
            name: lookup(store, "[method]test-case.name")?,
            run: lookup(store, "[method]test-case.run")?,
        })
    }

    async fn all<D: RunnerView + 'static>(&self, store: &mut Store<D>) -> Result<Vec<Val>> {
        let mut results = [Val::Bool(false)];
        self.all
            .call_async(&mut *store, &[], &mut results)
            .await
            .context("calling tests.all")?;
        match results.into_iter().next().unwrap() {
            Val::List(cases) => Ok(cases),
            other => bail!("unexpected tests.all result: {other:?}"),
        }
    }

    async fn name<D: RunnerView + 'static>(
        &self,
        store: &mut Store<D>,
        case: &Val,
    ) -> Result<String> {
        let mut results = [Val::Bool(false)];
        self.name
            .call_async(&mut *store, std::slice::from_ref(case), &mut results)
            .await
            .context("calling test-case.name")?;
        match results.into_iter().next().unwrap() {
            Val::String(s) => Ok(s),
            other => bail!("unexpected test-case.name result: {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trap_detail_extracts_root_trap_message() {
        let r: Result<()> = Err(format_err!(
            "wasm trap: wasm `unreachable` instruction executed"
        ));
        let e = r.context("running case `fixture/trap/boom`").unwrap_err();
        assert_eq!(
            trap_detail(&e),
            "wasm trap: wasm `unreachable` instruction executed"
        );
    }

    #[test]
    fn trap_detail_falls_back_to_first_line_one_liner() {
        let e = format_err!("component instantiation failed\nbecause of reasons\nand more");
        assert_eq!(trap_detail(&e), "component instantiation failed");
        // One-line convention: never multiline.
        assert!(!trap_detail(&e).contains('\n'));
    }
}
