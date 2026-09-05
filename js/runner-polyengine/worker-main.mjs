// The polyengine shard worker's message loop, shared by the stock worker
// (./browser-worker.mjs) and downstream repos' bundled workers. The case
// loop is ct-runner's `runSuite` — the same engine path the Deno leg
// (./runner.ts) drives — which mirrors js/viewer/harness.mjs semantics
// (striping, freshCases, timeouts, tag scheduling) and provides
// test-context itself. Per-shard rows are relayed as they are emitted;
// the envelope and terminator are dropped (the parent, page-runner.mjs,
// writes the single merged pair).
//
// A downstream conformance suite usually imports a SUT host module
// (`polymorph:websocket/connections`, `polymorph:webcrypto/*`, …) that
// the stock worker cannot supply: workers resolve no import maps, so the
// host module and the polyengine engine must arrive in ONE bundle or the
// embedder module loads twice and stateful handles (streams, futures)
// minted through one copy are refused by the other. The downstream pattern
// is a bundled worker entry:
//
//   // worker-entry.ts — deno bundle --platform browser
//   import * as polyengine from "./browser-bundle-entry.ts"; // jsr:@polyengine/*
//   import { workerMain } from "@polymorph/component-test-js/polyengine-worker-main";
//   import { configure, websocketImports } from "../../js/polyengine/websocket.ts";
//   workerMain({
//     polyengine,
//     suiteImports: ({ env }) => {
//       configure({ /* the leg's bounds */ });
//       return websocketImports();
//     },
//   });
//
// The bundler resolves every `@polyengine/runtime/embedder` in the graph to
// one module, so identity holds by construction; the run message's
// `bundleUrl` is unused when `polyengine` is passed (the engine is inlined).
//
// Run message and reply protocol are ./browser-worker.mjs's, unchanged:
//   { bundleUrl?, translatorUrl, suiteUrl, env?, missing?, only?, shard?,
//     caseTimeoutMs?, freshCases? (default true) }
//   -> { kind: "event", index, event } per case,
//      { kind: "counts", counts } on completion,
//      { kind: "error", error } on harness breakage.

async function fetchBytes(url) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`fetching ${url}: ${res.status}`);
  return new Uint8Array(await res.arrayBuffer());
}

/**
 * Install the shard worker's message handler.
 *
 * @param {object} [options]
 * @param {object} [options.polyengine]  The polyengine engine namespace (the
 *   embedder bundle's exports), already imported — bundled workers pass
 *   their inlined copy. Absent, each run message's `bundleUrl` is
 *   dynamically imported (the stock worker's behavior).
 * @param {(input: { polyengine: object, env: [string, string][] })
 *   => object | Promise<object>} [options.suiteImports]  Builds the
 *   SUT host-import record for one suite instance; merged over the
 *   engine's own wasi imports (`runSuite` wires test-context itself).
 *   Called once per run
 *   message (instances share module-level host state exactly as the
 *   repos' Deno legs do).
 */
export function workerMain({ polyengine, suiteImports } = {}) {
  self.onunhandledrejection = (event) => {
    event.preventDefault?.();
    self.postMessage({ kind: "error", error: String(event.reason?.stack ?? event.reason) });
  };

  self.onmessage = async ({ data }) => {
    const {
      bundleUrl,
      translatorUrl,
      suiteUrl,
      env = [],
      missing,
      only,
      shard,
      caseTimeoutMs,
      freshCases = true,
    } = data;
    try {
      const [translatorBytes, suiteBytes] = await Promise.all([
        fetchBytes(translatorUrl),
        fetchBytes(suiteUrl),
      ]);
      const bundle = polyengine ?? bundleUrl;
      const resolved = typeof bundle === "string" ? await import(bundle) : bundle;
      const hostImports = suiteImports
        ? await suiteImports({ polyengine: resolved, env })
        : undefined;

      const translator = await resolved.Translator.create(translatorBytes);
      const { plan, adapters } = translator.translate(suiteBytes);
      const artifacts = { plan, componentBytes: suiteBytes, adapters };
      const imports = {
        ...resolved.wasi({ cli: { env: Object.fromEntries(env) } }),
        // No test-context here: `runSuite` wires its own provider and
        // errors on a caller collision (ct-runner/src/run-suite.ts).
        ...hostImports,
      };

      const counts = await resolved.runSuite(artifacts, {
        imports,
        // The envelope this run message produces is discarded (the parent
        // page-runner writes the single merged envelope for all shards),
        // so target/suite name are placeholders, not run identity.
        target: "polyengine/worker",
        suiteName: "shard",
        only,
        missing,
        shard,
        caseTimeoutMs,
        // `freshCases: false` reuses one instance for the whole shard —
        // the documented trade for corpora whose per-case fresh instances
        // outrun the renderer's wasm-memory reservations (a trapped case
        // then poisons the rest of the shard, loudly).
        freshCases,
        emit: (line, caseIndex) => {
          // Envelope and `{"segment-end":true}` terminator carry no case
          // index; the parent writes its own, so drop them here.
          if (caseIndex === undefined) return;
          self.postMessage({ kind: "event", index: caseIndex, event: JSON.parse(line) });
        },
      });
      // `RunCounts.total` is this stripe's full case count, before `only`
      // filtering; the parent's empty-selection check reads `selected`.
      const selected = counts.passed + counts.failed + counts.skipped + counts.na;
      self.postMessage({
        kind: "counts",
        counts: { ...counts, selected, deselected: counts.total - selected },
      });
    } catch (err) {
      self.postMessage({ kind: "error", error: String(err?.stack ?? err) });
    }
  };
}
