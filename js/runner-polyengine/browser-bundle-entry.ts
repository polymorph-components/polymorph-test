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
export { Translator } from "@polyengine/runtime/shim";
export * from "@polyengine/ct-runner";
export { wasi } from "@polyengine/wasi";
export type { WasiImports, WasiOptions } from "@polyengine/wasi";
