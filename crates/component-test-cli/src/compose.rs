//! `compose-runner`: compose a suite with a context provider and the
//! wasi:cli runner core, as a library (wac-graph) instead of shelled-out
//! `wac` — the same graph as `examples/compose/compose.wac`.
//!
//! The topology needs a *shared* provider instance — the suite's
//! `test-context` import and the runner core's `factory`/`test-context`
//! imports must resolve to the same instance so the `context` resource
//! type is one type — which is exactly what `wac plug` cannot express
//! (see examples/compose/README.md). Pre-composed *bundles* (a suite
//! already `wac compose`d with its own providers, re-exporting `tests`
//! plus `test-context` plus `factory`) skip the provider and plug
//! straight into the runner core.
//!
//! The default provider and runner core are baked in at build time from
//! `embedded/` (size-optimized `embed`-profile builds of
//! `components/provider` and `components/runner-cli`; regenerate with
//! `just embed-update` and commit the diff — `just verify-cli` gates
//! their behavior against the same goldens as the wac-composed path).
//! Both are overridable per invocation.
//!
//! Composition strips custom sections (findings #14): the result is
//! execute-everything, its envelope says `scheduling: none`, and
//! lockfiles must be generated from the suite artifact, not from
//! anything this module produces.

use anyhow::{bail, Context as _, Result};
use wac_graph::types::Package;
use wac_graph::{CompositionGraph, EncodeOptions, NodeId, PackageId};

/// The wasi:cli runner core (`components/runner-cli`), `embed` profile.
pub const EMBEDDED_RUNNER: &[u8] = include_bytes!("../embedded/runner-cli.wasm");
/// The reference context provider (`components/provider`), `embed` profile.
pub const EMBEDDED_PROVIDER: &[u8] = include_bytes!("../embedded/provider.wasm");

/// The frozen contract interfaces (wit/tests.wit; L1) and the provider
/// interface the reference runner core consumes.
const TESTS: &str = "polymorph:test/tests@0.1.0";
const CONTEXT: &str = "polymorph:test/test-context@0.1.0";
const FACTORY: &str = "polymorph:test-provider/factory@0.1.0";
/// The composed entry point (version-prefix matched: the runner core
/// override decides the exact wasi:cli version).
const RUN_PREFIX: &str = "wasi:cli/run@";

/// What a composition input turned out to be, by its exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// Exports `tests` only: needs a provider and the runner core.
    Suite,
    /// Exports `tests` + `test-context` + `factory` (a suite already
    /// bundled with its providers): needs only the runner core.
    Bundle,
    /// Exports `wasi:cli/run`: already composed, nothing to do.
    Composed,
}

/// Read a component from disk, with the same crisp non-wasm error the
/// other subcommands give.
pub fn read_component(path: &str) -> Result<Vec<u8>> {
    let wasm = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    if !wasm.starts_with(b"\0asm") {
        bail!("{path} is not a WebAssembly binary (bad magic)");
    }
    Ok(wasm)
}

/// Classify `input` by its exports (see [`Input`]).
pub fn classify(input: &[u8]) -> Result<Input> {
    let mut graph = CompositionGraph::new();
    let pkg = register(&mut graph, "input", input)?;
    Ok(classify_registered(&graph, pkg))
}

fn classify_registered(graph: &CompositionGraph, pkg: PackageId) -> Input {
    let exports = &graph.types()[graph[pkg].ty()].exports;
    if exports.keys().any(|n| n.starts_with(RUN_PREFIX)) {
        Input::Composed
    } else if exports.contains_key(FACTORY) && exports.contains_key(CONTEXT) {
        Input::Bundle
    } else {
        Input::Suite
    }
}

fn register(graph: &mut CompositionGraph, name: &str, bytes: &[u8]) -> Result<PackageId> {
    let package = Package::from_bytes(name, None, bytes.to_vec(), graph.types_mut())
        .with_context(|| format!("parsing the {name} component"))?;
    graph
        .register_package(package)
        .with_context(|| format!("registering the {name} component"))
}

/// Wire every import of `to` that `from`'s instance exports by exact
/// name. Unwired imports (WASI) stay imports of the composition.
/// `aliases` caches export aliases so one exported instance feeds any
/// number of importers (the shared-provider requirement).
fn wire(
    graph: &mut CompositionGraph,
    aliases: &mut std::collections::HashMap<(NodeId, String), NodeId>,
    from: (NodeId, PackageId, &str),
    to: (NodeId, PackageId, &str),
) -> Result<()> {
    let (from_inst, from_pkg, from_name) = from;
    let (to_inst, to_pkg, to_name) = to;
    let exports: Vec<String> = graph.types()[graph[from_pkg].ty()]
        .exports
        .keys()
        .cloned()
        .collect();
    for name in exports {
        if !graph.types()[graph[to_pkg].ty()]
            .imports
            .contains_key(&name)
        {
            continue;
        }
        let alias = match aliases.entry((from_inst, name.clone())) {
            std::collections::hash_map::Entry::Occupied(e) => *e.get(),
            std::collections::hash_map::Entry::Vacant(e) => *e.insert(
                graph
                    .alias_instance_export(from_inst, &name)
                    .with_context(|| format!("aliasing `{name}` from the {from_name}"))?,
            ),
        };
        graph
            .set_instantiation_argument(to_inst, &name, alias)
            .with_context(|| {
                format!("plugging the {to_name}'s `{name}` import from the {from_name}")
            })?;
    }
    Ok(())
}

/// Compose `input` (a suite or a bundle; see [`Input`]) with `provider`
/// and the `runner` core into a runnable wasi:cli component.
pub fn compose(input: &[u8], provider: &[u8], runner: &[u8]) -> Result<Vec<u8>> {
    let mut graph = CompositionGraph::new();
    let input_pkg = register(&mut graph, "input", input)?;
    let runner_pkg = register(&mut graph, "runner-cli", runner)?;

    let input_kind = classify_registered(&graph, input_pkg);
    if input_kind == Input::Composed {
        bail!(
            "input already exports wasi:cli/run — it is already composed \
             (`component-test run` executes it directly)"
        );
    }
    if !graph.types()[graph[input_pkg].ty()]
        .exports
        .contains_key(TESTS)
    {
        bail!("input does not export `{TESTS}`: not a test suite (or bundle of one)");
    }
    // A partial bundle would wire ambiguously (whose context feeds the
    // runner core?); a real bundle exports both or neither.
    if input_kind == Input::Suite
        && (graph.types()[graph[input_pkg].ty()]
            .exports
            .contains_key(CONTEXT)
            || graph.types()[graph[input_pkg].ty()]
                .exports
                .contains_key(FACTORY))
    {
        bail!(
            "input exports one of `{CONTEXT}`/`{FACTORY}` but not both: \
             a bundle must re-export both (see examples/compose/bundle.wac)"
        );
    }

    let mut aliases = std::collections::HashMap::new();
    let runner_inst = graph.instantiate(runner_pkg);
    match input_kind {
        Input::Composed => unreachable!("rejected above: composed inputs do not export tests"),
        // Bundle: everything the runner core imports re-exports from
        // the one bundle instance (whose innards already share their
        // provider).
        Input::Bundle => {
            let bundle_inst = graph.instantiate(input_pkg);
            wire(
                &mut graph,
                &mut aliases,
                (bundle_inst, input_pkg, "bundle"),
                (runner_inst, runner_pkg, "runner core"),
            )?;
        }
        // Bare suite: one shared provider instance feeds the suite's
        // `test-context` import and the runner core's `factory` +
        // `test-context` imports (examples/compose/compose.wac).
        Input::Suite => {
            let provider_pkg = register(&mut graph, "provider", provider)?;
            let provider_inst = graph.instantiate(provider_pkg);
            let suite_inst = graph.instantiate(input_pkg);
            wire(
                &mut graph,
                &mut aliases,
                (provider_inst, provider_pkg, "provider"),
                (suite_inst, input_pkg, "suite"),
            )?;
            wire(
                &mut graph,
                &mut aliases,
                (provider_inst, provider_pkg, "provider"),
                (runner_inst, runner_pkg, "runner core"),
            )?;
            wire(
                &mut graph,
                &mut aliases,
                (suite_inst, input_pkg, "suite"),
                (runner_inst, runner_pkg, "runner core"),
            )?;
        }
    }

    let run_name = graph.types()[graph[runner_pkg].ty()]
        .exports
        .keys()
        .find(|n| n.starts_with(RUN_PREFIX))
        .cloned()
        .with_context(|| format!("runner core exports no `{RUN_PREFIX}...` — not a runner core"))?;
    let run = graph
        .alias_instance_export(runner_inst, &run_name)
        .context("aliasing the runner core's run export")?;
    graph
        .export(run, &run_name)
        .context("exporting wasi:cli/run")?;

    graph
        .encode(EncodeOptions::default())
        .context("encoding the composition")
}
