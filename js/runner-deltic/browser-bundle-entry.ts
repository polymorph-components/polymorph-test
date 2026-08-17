// The browser-leg bundle entry: one platform-neutral ES module carrying
// the embedder API + Translator + ct-runner glue + wasi shims, bundled
// from the SAME pinned JSR graph as the Deno leg (deno.json + deno.lock,
// --frozen). It replaces the sha-pinned `deltic-embedder.mjs` release
// asset the retired release-asset fetch script downloaded; the surface is
// upstream tools/release-bundle/entry.ts's, verbatim.
//
//   deno bundle --config js/runner-deltic/deno.json --frozen \
//       --platform browser -o target/deltic-browser/deltic-embedder.mjs \
//       js/runner-deltic/browser-bundle-entry.ts
//
// (`just deltic-assets` builds it; verify-deltic/viewer-build consume it.)

export * from "@deltic/runtime/embedder";
export { Translator } from "@deltic/runtime/shim";
export * from "@deltic/ct-runner";
export { wasi } from "@deltic/wasi";
export type { WasiImports, WasiOptions } from "@deltic/wasi";
