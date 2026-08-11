// The deltic browser-side shard worker — the runtime-linked sibling of
// js/viewer/browser-worker.mjs (same reply protocol, same harness case
// loop, no transpiled artifacts). Drop-in for page-runner.mjs's
// runSuitesInPage via its workerUrl parameter; suite entries carry this
// worker's run message instead of the jco one:
//
//   {
//     bundleUrl,      // deltic-embedder.mjs, built from the pinned JSR
//                     // graph by `just deltic-assets`
//     translatorUrl,  // deltic-translator-shim.wasm, extracted from the
//                     // same locked graph (@deltic/translator's asset)
//     suiteUrl,       // the suite COMPONENT wasm (no transpile, no cores)
//     env?,           // [name, value] pairs for wasi:cli/environment
//     missing?, only?, shard?, caseTimeoutMs?,
//   }
//
// Replies: { kind: "event", index, event } per case,
// { kind: "counts", counts } on completion, { kind: "error", error }
// on harness breakage.
//
// Suites that import a SUT host module need a bundled worker instead:
// see ./worker-main.mjs, whose message loop this worker installs with
// the stock (no-SUT, bundleUrl-loading) configuration.

import { workerMain } from "./worker-main.mjs";

workerMain();
