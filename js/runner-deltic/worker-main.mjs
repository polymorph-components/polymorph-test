// The deltic shard worker's message loop, shared by the stock worker
// (./browser-worker.mjs) and downstream repos' bundled workers.
//
// A downstream conformance suite usually imports a SUT host module
// (`polymorph:websocket/connections`, `polymorph:webcrypto/*`, …) that
// the stock worker cannot supply: workers resolve no import maps, so the
// host module and the deltic engine must arrive in ONE bundle or the
// embedder module loads twice and `instanceof WitError` stops holding
// across the boundary. The downstream pattern is a bundled worker entry:
//
//   // worker-entry.ts — deno bundle --platform browser
//   import * as deltic from "…/tools/release-bundle/entry.ts"; // raw URLs
//   import { workerMain } from "@polymorph/component-test-js/deltic-worker-main";
//   import { configure, websocketImports } from "../../js/deltic/websocket.ts";
//   workerMain({
//     deltic,
//     suiteImports: ({ env }) => {
//       configure({ /* the leg's bounds */ });
//       return websocketImports();
//     },
//   });
//
// The bundler resolves every `@deltic/runtime/embedder` in the graph to
// one module, so identity holds by construction; the run message's
// `bundleUrl` is unused when `deltic` is passed (the engine is inlined).
//
// Run message and reply protocol are ./browser-worker.mjs's, unchanged:
//   { bundleUrl?, translatorUrl, suiteUrl, env?, missing?, only?, shard?,
//     caseTimeoutMs? }
//   -> { kind: "event", index, event } per case,
//      { kind: "counts", counts } on completion,
//      { kind: "error", error } on harness breakage.

import { runCases } from "../viewer/harness.mjs";
import { loadSuite } from "./engine.mjs";

async function fetchBytes(url) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`fetching ${url}: ${res.status}`);
  return new Uint8Array(await res.arrayBuffer());
}

/**
 * Install the shard worker's message handler.
 *
 * @param {object} [options]
 * @param {object} [options.deltic]  The deltic engine namespace (the
 *   embedder bundle's exports), already imported — bundled workers pass
 *   their inlined copy. Absent, each run message's `bundleUrl` is
 *   dynamically imported (the stock worker's behavior).
 * @param {(input: { deltic: object, env: [string, string][] })
 *   => object | Promise<object>} [options.suiteImports]  Builds the
 *   SUT host-import record for one suite instance; merged over the
 *   engine's own wasi + test-context imports. Called once per run
 *   message (instances share module-level host state exactly as the
 *   repos' Deno legs do).
 */
export function workerMain({ deltic, suiteImports } = {}) {
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
      missing = [],
      only,
      shard,
      caseTimeoutMs,
    } = data;
    try {
      const [translatorBytes, suiteBytes] = await Promise.all([
        fetchBytes(translatorUrl),
        fetchBytes(suiteUrl),
      ]);
      const bundle = deltic ?? bundleUrl;
      const resolved = typeof bundle === "string" ? await import(bundle) : bundle;
      const hostImports = suiteImports
        ? await suiteImports({ deltic: resolved, env })
        : undefined;
      const { newTests, Context, tagsOf } = await loadSuite({
        bundle: resolved,
        translatorBytes,
        suiteBytes,
        env,
        hostImports,
      });

      const counts = await runCases({
        cases: await (await newTests()).all(),
        Context,
        tagsOf,
        missing,
        only,
        shard,
        caseTimeoutMs,
        emit: (event, index) => self.postMessage({ kind: "event", index, event }),
        freshCases: async () => (await newTests()).all(),
      });
      self.postMessage({ kind: "counts", counts });
    } catch (err) {
      self.postMessage({ kind: "error", error: String(err?.stack ?? err) });
    }
  };
}
