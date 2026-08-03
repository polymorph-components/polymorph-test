//! Host-embedding runner for `lann:component-test` suites.
//!
//! Loads a suite component (world `suite` from `wit/tests.wit`), provides
//! its `test-context` import host-side, and runs every case in a fresh
//! instance (instance-per-case). The suite's exports are async-lifted
//! (component-model-async), so calls go through wasmtime's concurrent
//! API; host-side `diagnostic` is a concurrent host function.

use std::path::Path;

use component_test_core::{Marks, Provenance};
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

/// Store data: WASI plus the per-case diagnostic sink.
pub struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
    /// Diagnostics reported by the currently running case.
    diagnostics: Vec<String>,
    /// Print each diagnostic as it arrives (human mode).
    live_print: bool,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
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

pub struct Runner {
    engine: Engine,
    component: Component,
    linker: Linker<Ctx>,
    wasm_bytes: Vec<u8>,
}

impl Runner {
    pub fn new(suite_path: &Path) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.wasm_component_model_async(true);
        let engine = Engine::new(&config)?;

        let wasm_bytes = std::fs::read(suite_path)
            .with_context(|| format!("reading suite component {}", suite_path.display()))?;
        let component = Component::new(&engine, &wasm_bytes)
            .with_context(|| format!("loading suite component {}", suite_path.display()))?;

        let mut linker: Linker<Ctx> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;

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
                        let data = access.data_mut();
                        if data.live_print {
                            println!("    diag: {msg}");
                        }
                        data.diagnostics.push(msg);
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
        })
    }

    fn new_store(&self, live_print: bool) -> Result<Store<Ctx>> {
        let wasi = WasiCtxBuilder::new().inherit_stderr().build();
        Ok(Store::new(
            &self.engine,
            Ctx {
                wasi,
                table: ResourceTable::new(),
                diagnostics: Vec::new(),
                live_print,
            },
        ))
    }

    async fn instantiate(&self, store: &mut Store<Ctx>) -> Result<Instance> {
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

    /// Run case `index` in its own fresh instance. Returns the name (from
    /// this instance) and what happened.
    async fn run_case(
        &self,
        index: usize,
        live_print: bool,
    ) -> Result<(String, Verdict, Vec<String>)> {
        let mut store = self.new_store(live_print)?;
        let instance = self.instantiate(&mut store).await?;
        let funcs = TestsFuncs::new(&mut store, &instance)?;

        let cases = funcs.all(&mut store).await?;
        let case = cases
            .get(index)
            .cloned()
            .ok_or_else(|| format_err!("case index {index} out of range on re-enumeration"))?;
        let name = funcs.name(&mut store, &case).await?;

        // Host context resource for this case, lent to the guest as a
        // borrow.
        let ctx = Resource::<HostContext>::new_own(0);
        let ctx_any = ctx.try_into_resource_any(&mut store)?;

        let mut results = [Val::Bool(false)];
        let call = funcs
            .run
            .call_async(&mut store, &[case, Val::Resource(ctx_any)], &mut results)
            .await;

        let diagnostics = std::mem::take(&mut store.data_mut().diagnostics);

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
        let human = matches!(mode, OutputMode::Human);

        // Static inventory (marks) from the suite artifact, if present.
        let inventory: Option<std::collections::BTreeMap<String, Marks>> =
            match component_test_formats::inventory::inventory(&self.wasm_bytes) {
                Ok(entries) => Some(
                    entries
                        .into_iter()
                        .map(|e| {
                            (
                                e.name.as_str().to_string(),
                                Marks::new(e.marks).expect("validated by inventory parse"),
                            )
                        })
                        .collect(),
                ),
                Err(_) => None,
            };

        if !human {
            let envelope = Envelope {
                version: RESULTS_VERSION.into(),
                target: "wasmtime/host".into(),
                suite: SuiteInfo {
                    name: suite_name.into(),
                    ..Default::default()
                },
                run: RunInfo::default(),
            };
            println!("{}", serde_json::to_string(&envelope)?);
        }

        let names = self.enumerate().await.context("enumerating suite")?;
        let mut summary = Summary::default();

        // Runtime cross-check: the static inventory and `all()` must
        // agree (drift = harness bug).
        if let Some(inv) = &inventory {
            let enumerated: std::collections::BTreeSet<&str> =
                names.iter().map(|s| s.as_str()).collect();
            let recorded: std::collections::BTreeSet<&str> =
                inv.keys().map(|s| s.as_str()).collect();
            if enumerated != recorded {
                bail!(
                    "inventory drift: marks section and all() disagree \
                     (section-only: {:?}; all()-only: {:?})",
                    recorded.difference(&enumerated).collect::<Vec<_>>(),
                    enumerated.difference(&recorded).collect::<Vec<_>>(),
                );
            }
        }

        for (index, enumerated_name) in names.iter().enumerate() {
            // Scheduler: skip cases that do not apply to this target.
            if let Some(inv) = &inventory {
                if let Some(marks) = inv.get(enumerated_name) {
                    if !marks.applies(missing_features) {
                        let mark = marks
                            .excluding_mark(missing_features)
                            .map(|m| m.to_string())
                            .unwrap_or_default();
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
                }
            }

            if human {
                println!("test {enumerated_name} ...");
            }

            let (name, verdict, diagnostics) = match self.run_case(index, human).await {
                Ok(r) => r,
                Err(e) => (
                    enumerated_name.clone(),
                    Verdict::Trap(format!("{e:#}")),
                    Vec::new(),
                ),
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

/// The suite's `tests` export surface, looked up dynamically.
struct TestsFuncs {
    all: Func,
    name: Func,
    run: Func,
}

impl TestsFuncs {
    fn new(store: &mut Store<Ctx>, instance: &Instance) -> Result<Self> {
        let (_, tests) = instance
            .get_export(&mut *store, None, TESTS_INSTANCE)
            .ok_or_else(|| format_err!("suite does not export `{TESTS_INSTANCE}`"))?;
        let lookup = |store: &mut Store<Ctx>, name: &str| -> Result<Func> {
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

    async fn all(&self, store: &mut Store<Ctx>) -> Result<Vec<Val>> {
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

    async fn name(&self, store: &mut Store<Ctx>, case: &Val) -> Result<String> {
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
