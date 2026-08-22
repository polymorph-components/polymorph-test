// Runtime-linked viewer engines (the jco replacement's Phase 4): the
// polyengine browser assets (repo-built from the pinned JSR graph — see
// js/runner-polyengine/README.md) are deployed beside the viewer by
// `just viewer-build` (locally and on Pages alike — same relative
// layout), and the aggregation component is instantiated at first use.
//
//   ./polyengine/polyengine-embedder.mjs         one platform-neutral ES module
//   ./polyengine/polyengine-translator-shim.wasm the runtime linker's translator
//   ./generated/viewer-aggregate.wasm    the aggregation COMPONENT
//                                        (the gate's own Rust — no
//                                        transpile step anymore)
//
// Browser-only (fetch-based); js/viewer/selftest.mjs builds the same
// engine under node with fs reads.

const ASSETS = new URL("./polyengine/", import.meta.url);
export const bundleUrl = new URL("polyengine-embedder.mjs", ASSETS).href;
export const translatorUrl = new URL("polyengine-translator-shim.wasm", ASSETS).href;

async function fetchBytes(url) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`fetching ${url}: ${res.status}`);
  return new Uint8Array(await res.arrayBuffer());
}

let enginePromise;

/**
 * The aggregation engine: `(lock, manifest, [[target, jsonl], …]) ->
 * Promise<json string>`. Instantiated once, lazily — polyengine translates
 * the component in-page (~ms; no generated JS anywhere, CSP baseline
 * `wasm-unsafe-eval` only). A guest-side `result::err` surfaces as a
 * thrown error with `.payload` carrying the message, exactly like the
 * transpiled engine it replaces.
 */
export function aggregateEngine() {
  enginePromise ??= (async () => {
    const polyengine = await import(bundleUrl);
    const [translatorBytes, componentBytes] = await Promise.all([
      fetchBytes(translatorUrl),
      fetchBytes(new URL("./generated/viewer-aggregate.wasm", import.meta.url)),
    ]);
    const translator = await polyengine.Translator.create(translatorBytes);
    const { plan, adapters } = translator.translate(componentBytes);
    const inst = await polyengine.instantiate(
      { plan, componentBytes, adapters },
      polyengine.wasi(),
    );
    return inst.exports.run;
  })();
  return enginePromise;
}
