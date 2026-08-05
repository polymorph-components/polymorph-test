//! Host-embedding runner for `lann:component-test` suites.
//!
//! Loads a suite component (world `suite` from `wit/tests.wit`), provides
//! its `test-context` import host-side, and runs every case in a fresh
//! instance (instance-per-case). The suite's exports are async-lifted
//! (component-model-async), so calls go through wasmtime's concurrent
//! API; host-side `diagnostic` is a concurrent host function.
//!
//! Env knobs: `COMPONENT_TEST_PROFILE=1` prints per-session enumeration
//! timings to stderr (double-enumerates each session to separate
//! registry construction from lifting cost).

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
    /// Executing-thread CPU time accumulated in the current budget
    /// phase (enumeration, or one case). Sampled at each epoch-deadline
    /// callback: the callback only fires while wasm executes, and the
    /// delta between consecutive samples on the same thread is the
    /// case's own execution regardless of the box's load. Counting the
    /// *ticks* themselves is not contention-robust: ticks are global
    /// wall clock, and a CPU-bound case under scheduler time-slicing
    /// executes inside essentially every tick interval, so tick counts
    /// degenerate to wall time exactly when the machine is contended
    /// (observed downstream: an honestly-slow PBKDF2 case tripping the
    /// budget at 8 jobs on 2 cores while using well under its budget
    /// of CPU).
    budget_used: std::time::Duration,
    /// The previous sample: (thread, its CPU clock). A thread change
    /// re-baselines instead of charging a bogus cross-thread delta
    /// (defensive: workers poll their stores on dedicated threads).
    budget_last: Option<(std::thread::ThreadId, std::time::Duration)>,
    /// CPU-time budget for the phase, seconds; 0 = unlimited.
    budget_max_secs: u64,
}

impl CtCtx {
    fn start_budget_phase(&mut self) {
        self.budget_used = std::time::Duration::ZERO;
        self.budget_last = None;
    }
}

/// The executing thread's CPU time (`CLOCK_THREAD_CPUTIME_ID`).
/// `None` on platforms without it; the budget then falls back to
/// charging one tick per callback (the wall-approximating behavior).
#[cfg(unix)]
fn thread_cpu_time() -> Option<std::time::Duration> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid out-pointer for the duration of the call.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    (rc == 0).then(|| std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32))
}

#[cfg(not(unix))]
fn thread_cpu_time() -> Option<std::time::Duration> {
    None
}

/// Raised by the epoch-deadline callback when a phase exceeds its
/// execution budget; discriminated by downcast after it unwinds
/// through the trap plumbing.
#[derive(Debug)]
struct ExecutionBudgetExceeded;

impl std::fmt::Display for ExecutionBudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("execution budget exceeded")
    }
}

impl std::error::Error for ExecutionBudgetExceeded {}

/// Epoch tick granularity: the ticker thread increments the engine
/// epoch at this interval, so budgets have ± one tick of slop and
/// wasm pays one deadline check per tick.
const EPOCH_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// Default `--case-execution-budget` (seconds of wasm execution).
pub const DEFAULT_CASE_EXECUTION_BUDGET_SECS: u64 = 10;
/// Default `--case-timeout` (seconds of wall clock per case).
pub const DEFAULT_CASE_TIMEOUT_SECS: u64 = 120;

/// Keeps the engine epoch advancing while budgets are armed; stops the
/// ticker thread on drop.
struct EpochTicker {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl EpochTicker {
    fn spawn(engine: &Engine) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = {
            let engine = engine.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(EPOCH_TICK);
                    engine.increment_epoch();
                }
            })
        };
        EpochTicker {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
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
    /// The runner killed the case for exceeding a limit and abandoned
    /// the instance: (limit kind — the `limit-exceeded` payload
    /// vocabulary, one-line detail).
    Limit(&'static str, String),
}

pub struct Runner<D: RunnerView + 'static = Ctx> {
    engine: Engine,
    component: Component,
    linker: Linker<D>,
    wasm_bytes: Vec<u8>,
    /// Overrides the envelope's artifact binding (see
    /// [`Runner::bind_suite_artifact`]). `None` binds the executed
    /// artifact itself.
    suite_artifact_sha256: Option<String>,
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
        // Always instrumented: the deadline checks are how the
        // execution budget regains control from a CPU-spinning guest
        // (a host-side timer cannot fire while the executor thread is
        // stuck inside wasm). Cost is a check per tick; with budgets
        // disabled the epoch never advances and no deadline ever
        // trips.
        config.epoch_interruption(true);
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
            suite_artifact_sha256: None,
            make_data: Box::new(make_data),
        })
    }

    /// Bind the results envelope to `suite_wasm` — the *suite*
    /// component's bytes — instead of the executed artifact. For
    /// composed runs (suite `wac plug`ged with its providers) the
    /// executed bundle's hash can never match the suite's lockfile,
    /// which records the suite artifact (the lockfile side of finding
    /// #14); the caller attests the bundle was composed from exactly
    /// these bytes. Case scheduling and drift checks still read the
    /// executed artifact's inventory.
    pub fn bind_suite_artifact(&mut self, suite_wasm: &[u8]) {
        self.suite_artifact_sha256 = Some(component_test_formats::sha256_hex(suite_wasm));
    }

    fn new_store(&self, live_print: bool, budget_max_secs: u64) -> Result<Store<D>> {
        let mut store = Store::new(&self.engine, (self.make_data)());
        {
            let ct = store.data_mut().ct();
            ct.live_print = live_print;
            ct.budget_max_secs = budget_max_secs;
        }
        // Armed before any wasm runs (instantiation executes guest
        // code). The callback samples the executing thread's CPU clock
        // against the current phase's budget; the driver resets the
        // accounting per phase (enumeration, then each case).
        store.epoch_deadline_callback(|mut cx| {
            let now = thread_cpu_time();
            let tid = std::thread::current().id();
            let ct = cx.data_mut().ct();
            match (now, ct.budget_last) {
                // Same thread, sane clock: charge the case its own
                // execution since the last sample.
                (Some(now), Some((last_tid, last))) if last_tid == tid && now >= last => {
                    ct.budget_used += now - last;
                    ct.budget_last = Some((tid, now));
                }
                // First sample of the phase, or a thread change:
                // (re)baseline without charging.
                (Some(now), _) => ct.budget_last = Some((tid, now)),
                // No thread CPU clock on this platform: fall back to
                // charging one tick per callback (approximates wall
                // while executing).
                (None, _) => ct.budget_used += EPOCH_TICK,
            }
            if ct.budget_max_secs != 0
                && ct.budget_used >= std::time::Duration::from_secs(ct.budget_max_secs)
            {
                Err(ExecutionBudgetExceeded.into())
            } else {
                Ok(wasmtime::UpdateDeadline::Continue(1))
            }
        });
        store.set_epoch_deadline(1);
        Ok(store)
    }

    async fn instantiate(&self, store: &mut Store<D>) -> Result<Instance> {
        self.linker
            .instantiate_async(&mut *store, &self.component)
            .await
    }

    /// Enumerate the suite in a fresh instance: case names, in suite order.
    pub async fn enumerate(&self) -> Result<Vec<String>> {
        self.enumerate_with(0).await
    }

    async fn enumerate_with(&self, budget_max_secs: u64) -> Result<Vec<String>> {
        let mut store = self.new_store(false, budget_max_secs)?;
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
    async fn new_session(&self, live_print: bool, budget_max_secs: u64) -> Result<Session<D>> {
        let mut store = self.new_store(live_print, budget_max_secs)?;
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

    /// Serve one case through the session slot: recycle the session at
    /// the instance-granularity knob, (re)create it as needed, run the
    /// case, and poison the slot on abandonment-class verdicts. The
    /// single implementation behind both the sequential and worker
    /// loops — the two had already diverged once (session-creation
    /// failure policy), so per-case logic lives here or nowhere.
    ///
    /// Session-creation failure is a per-case trap + continue (the
    /// worker path's policy, now unified): poisoning is contained, and
    /// a suite whose constructor wedges deterministically never gets
    /// here — the census enumeration runs first and fails the whole
    /// run.
    #[allow(clippy::too_many_arguments)]
    async fn serve_case(
        &self,
        session: &mut Option<Session<D>>,
        live_print: bool,
        cases_per_instance: usize,
        index: usize,
        enumerated_name: &str,
        exec_budget_secs: u64,
        case_timeout_secs: u64,
    ) -> (String, Verdict, Vec<String>) {
        if session
            .as_ref()
            .is_some_and(|s| cases_per_instance != 0 && s.served >= cases_per_instance)
        {
            *session = None;
        }
        if session.is_none() {
            let created = with_wall_timeout(
                self.new_session(live_print, exec_budget_secs),
                case_timeout_secs,
            )
            .await;
            match created {
                Some(Ok(s)) => *session = Some(s),
                Some(Err(e)) => {
                    let verdict = limit_from_error(&e, exec_budget_secs)
                        .unwrap_or_else(|| Verdict::Trap(trap_detail(&e)));
                    return (enumerated_name.to_string(), verdict, Vec::new());
                }
                None => {
                    return (
                        enumerated_name.to_string(),
                        Verdict::Limit(
                            "case-timeout",
                            format!(
                                "session creation exceeded case timeout ({case_timeout_secs}s)"
                            ),
                        ),
                        Vec::new(),
                    );
                }
            }
        }
        let run = with_wall_timeout(
            self.run_case(session.as_mut().unwrap(), index),
            case_timeout_secs,
        )
        .await;
        let result = match run {
            Some(Ok(r)) => r,
            Some(Err(e)) => {
                let verdict = limit_from_error(&e, exec_budget_secs)
                    .unwrap_or_else(|| Verdict::Trap(trap_detail(&e)));
                (enumerated_name.to_string(), verdict, Vec::new())
            }
            None => {
                // The dropped call left the instance wounded; salvage
                // the diagnostics that made it out before abandoning.
                let diagnostics = session
                    .as_mut()
                    .map(|s| std::mem::take(&mut s.store.data_mut().ct().diagnostics))
                    .unwrap_or_default();
                (
                    enumerated_name.to_string(),
                    Verdict::Limit(
                        "case-timeout",
                        format!("case timeout exceeded ({case_timeout_secs}s)"),
                    ),
                    diagnostics,
                )
            }
        };
        if matches!(result.1, Verdict::Trap(_) | Verdict::Limit(..)) {
            // Poisoned or abandoned: never reuse the instance.
            *session = None;
        }
        result
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
        // Each case gets a fresh execution-budget phase.
        store.data_mut().ct().start_budget_phase();
        let exec_budget_secs = store.data_mut().ct().budget_max_secs;
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
            Err(e) => limit_from_error(&e, exec_budget_secs)
                .unwrap_or_else(|| Verdict::Trap(trap_detail(&e))),
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
            DEFAULT_CASE_EXECUTION_BUDGET_SECS,
            DEFAULT_CASE_TIMEOUT_SECS,
        )
        .await
    }

    /// `only`: run only cases whose name contains the substring (a
    /// dev-loop filter; filtered cases are omitted from output, so
    /// filtered runs will not aggregate cleanly — by design).
    ///
    /// `case_execution_budget_secs`: budget on actual wasm *execution*
    /// per case — the executing thread's CPU time, sampled at epoch
    /// ticks, so a contended machine stretching a case's wall clock
    /// does not eat its budget. Catches CPU spins, which a wall timer
    /// cannot (the executor thread is stuck inside wasm). `0` disables.
    ///
    /// `case_timeout_secs`: wall clock per case, suspension included.
    /// Catches async wedges (a case awaiting something that never
    /// resolves), which the execution budget cannot (no wasm runs).
    /// `0` disables.
    ///
    /// Either trip fails the case with provenance
    /// `limit-exceeded(<kind>)` and abandons the instance — the same
    /// containment as a trap; the next case gets a fresh session. No
    /// retries, per policy.
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
        case_execution_budget_secs: u64,
        case_timeout_secs: u64,
    ) -> Result<Summary> {
        let human = matches!(mode, OutputMode::Human);

        // The engine epoch only advances while a budget needs it; the
        // ticker stops when the run ends.
        let _ticker = (case_execution_budget_secs > 0).then(|| EpochTicker::spawn(&self.engine));

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
        let tags_of = {
            // O(1) exact lookup + short prefix-list fallback, built
            // once: the pre-pass below consults tags three times per
            // name (drift check, decline-pair, plan), which at
            // conformance-corpus scale (~10^4 cases) made the old
            // linear scan quadratic. `Tags` clones are refcount bumps.
            let exact: Option<std::collections::HashMap<std::borrow::Cow<'_, str>, Tags>> =
                inventory.as_ref().map(|inv| {
                    inv.cases
                        .iter()
                        .map(|e| {
                            (
                                e.name.as_str(),
                                Tags::new(e.tags.clone()).expect("validated by inventory parse"),
                            )
                        })
                        .collect()
                });
            let generated: Option<Vec<(&str, Tags)>> = inventory.as_ref().map(|inv| {
                inv.generated
                    .iter()
                    .map(|g| {
                        (
                            g.prefix.as_str(),
                            Tags::new(g.tags.clone()).expect("validated by inventory parse"),
                        )
                    })
                    .collect()
            });
            move |name: &str| -> Option<Tags> {
                let exact = exact.as_ref()?;
                if let Some(tags) = exact.get(name) {
                    return Some(tags.clone());
                }
                generated
                    .as_ref()?
                    .iter()
                    .filter(|(prefix, _)| component_test_core::name::is_under(name, prefix))
                    .max_by_key(|(prefix, _)| prefix.len())
                    .map(|(_, tags)| tags.clone())
            }
        };

        if !human {
            let envelope =
                Envelope {
                    version: RESULTS_VERSION.into(),
                    target: target.into(),
                    suite: SuiteInfo {
                        name: suite_name.into(),
                        // Binds the results to the exact suite build;
                        // `aggregate` cross-checks it against the lockfile.
                        // A composed run overrides this with the suite
                        // component's own hash (`bind_suite_artifact`).
                        artifact_sha256: Some(self.suite_artifact_sha256.clone().unwrap_or_else(
                            || component_test_formats::sha256_hex(&self.wasm_bytes),
                        )),
                        ..Default::default()
                    },
                    run: RunInfo {
                        // Without an inventory this runner is
                        // execute-everything too (composed bundles: wac
                        // strips the tags section); say so, so the
                        // aggregator applies applicability instead of
                        // policing it.
                        scheduling: Some(if inventory.is_some() { "tags" } else { "none" }.into()),
                        ..Default::default()
                    },
                };
            println!("{}", serde_json::to_string(&envelope)?);
        }

        // The census enumeration runs the suite's registry
        // constructor: guard it with the same budgets. A trip here is
        // a run error (nothing has executed a case yet), which also
        // means a deterministically wedged constructor fails the run
        // once instead of once per case.
        let names = match with_wall_timeout(
            self.enumerate_with(case_execution_budget_secs),
            case_timeout_secs,
        )
        .await
        {
            None => bail!("suite enumeration exceeded case timeout ({case_timeout_secs}s)"),
            Some(Err(e)) if limit_from_error(&e, case_execution_budget_secs).is_some() => bail!(
                "suite enumeration exceeded execution budget \
                 ({case_execution_budget_secs}s wasm execution)"
            ),
            Some(r) => r.context("enumerating suite")?,
        };
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
            let materialized: Vec<Tags> = names.iter().filter_map(|n| tags_of(n)).collect();
            let unpaired = component_test_core::tags::unpaired_positive_features(
                materialized.iter().map(|t| t.as_slice()),
            );
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
        // Same normative rule as the zero-enumeration guard above: a
        // `--only` substring matching nothing is an empty selection (a
        // typo'd filter must not exit green with "0 total").
        if plan.is_empty() {
            bail!(
                "--only `{}` matches no cases (empty selection is a run error)",
                only.unwrap_or_default()
            );
        }

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
                                let result = self
                                    .serve_case(
                                        &mut session,
                                        false,
                                        cases_per_instance,
                                        index,
                                        &names[index],
                                        case_execution_budget_secs,
                                        case_timeout_secs,
                                    )
                                    .await;
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
                self.serve_case(
                    &mut session,
                    human,
                    cases_per_instance,
                    index,
                    enumerated_name,
                    case_execution_budget_secs,
                    case_timeout_secs,
                )
                .await
            };

            if human {
                match &verdict {
                    Verdict::Pass => println!("test {name}: PASS"),
                    Verdict::Fail(d) => println!("test {name}: FAIL: {d}"),
                    Verdict::Skip(d) => println!("test {name}: SKIP: {d}"),
                    Verdict::Trap(d) => println!("test {name}: FAIL: trap: {d}"),
                    Verdict::Limit(_, d) => println!("test {name}: FAIL: {d}"),
                }
            } else {
                let (status, provenance, detail, complete) = match &verdict {
                    Verdict::Pass => (Status::Pass, Provenance::Returned, None, true),
                    Verdict::Fail(d) => (Status::Fail, Provenance::Returned, Some(d.clone()), true),
                    Verdict::Skip(d) => {
                        (Status::Skipped, Provenance::Returned, Some(d.clone()), true)
                    }
                    Verdict::Trap(d) => (Status::Fail, Provenance::Trap, Some(d.clone()), false),
                    Verdict::Limit(kind, d) => (
                        Status::Fail,
                        Provenance::LimitExceeded(kind.to_string()),
                        Some(d.clone()),
                        false,
                    ),
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
                Verdict::Fail(_) | Verdict::Trap(_) | Verdict::Limit(..) => summary.failed += 1,
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

/// Race `fut` against a wall-clock deadline (`0` = no deadline);
/// `None` = timed out. The loser is dropped — callers abandon the
/// session afterwards, so a wounded in-flight call is never resumed.
async fn with_wall_timeout<T>(
    fut: impl std::future::Future<Output = T>,
    timeout_secs: u64,
) -> Option<T> {
    if timeout_secs == 0 {
        return Some(fut.await);
    }
    use futures::FutureExt as _;
    let mut fut = std::pin::pin!(fut.fuse());
    let mut deadline =
        std::pin::pin!(tokio::time::sleep(std::time::Duration::from_secs(timeout_secs)).fuse());
    futures::select_biased! {
        r = fut => Some(r),
        _ = deadline => None,
    }
}

/// If `e` is (or wraps) the execution-budget marker, the corresponding
/// verdict; the Display fallback covers any wrapping that defeats
/// downcast through wasmtime's trap plumbing.
fn limit_from_error(e: &wasmtime::Error, exec_budget_secs: u64) -> Option<Verdict> {
    let is_budget = e.downcast_ref::<ExecutionBudgetExceeded>().is_some()
        || format!("{e:#}").contains("execution budget exceeded");
    is_budget.then(|| {
        Verdict::Limit(
            "execution-budget",
            format!("execution budget exceeded ({exec_budget_secs}s wasm execution)"),
        )
    })
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
