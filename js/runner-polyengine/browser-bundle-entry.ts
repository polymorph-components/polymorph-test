// The browser-leg bundle entry: one platform-neutral ES module carrying
// the embedder API + Translator + ct-runner glue + wasi shims, bundled
// from the SAME pinned JSR graph as the Deno leg (deno.json + deno.lock,
// --frozen). It replaces the sha-pinned `polyengine-embedder.mjs` release
// asset the retired release-asset fetch script downloaded; the surface is
// upstream tools/release-bundle/entry.ts's, verbatim.
//
//   deno bundle --config js/runner-polyengine/deno.json --frozen \
//       --platform browser -o target/polyengine-browser/polyengine-embedder.mjs \
//       js/runner-polyengine/browser-bundle-entry.ts
//
// (`just polyengine-assets` builds it; verify-polyengine/viewer-build consume it.)

export * from "@polyengine/runtime/embedder";
// A22: @polyengine/runtime@0.5.0's embedder dropped its A9 courtesy
// re-exports (error classes/predicates, brands, handle classes, suspending,
// realm crossing, copy registry) — that vocabulary now lives only in
// @polyengine/protocol. worker-main.mjs's and README.md's "Browser leg"
// section document downstream bundled workers relying on `ComponentException`
// (and, by the same contract, the rest of the vocabulary) being reachable
// off this bundle's exports alongside the embedder machinery, so it is
// re-exported here too. No name collisions: the embedder no longer exports
// these names (contracts/embedder-api.md A22; brief rule 5).
export * from "@polyengine/protocol";
export { Translator } from "@polyengine/runtime/shim";
export * from "@polyengine/ct-runner";
export { wasi } from "@polyengine/wasi";
export type { WasiImports, WasiOptions } from "@polyengine/wasi";
