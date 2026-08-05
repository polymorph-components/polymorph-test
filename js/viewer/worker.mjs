// The Web Worker half of the viewer's live run: its own instance of
// the transpiled suite runs one shard of the case loop (harness.mjs),
// streaming each results-JSONL event back with its suite-order index,
// then the shard's counts. Module workers cannot see a page's import
// map, which is why the viewer's transpiles map every import to a
// relative path.
import { inventoryLookup, runCases } from "./harness.mjs";
import { Context } from "./context.js";

// A rejection escaping the awaited chain would otherwise leave the
// worker silently wedged: unhandled rejections fire neither the catch
// below nor the page's worker.onerror.
self.onunhandledrejection = (event) => {
  event.preventDefault?.();
  self.postMessage({ kind: "error", error: String(event.reason?.stack ?? event.reason) });
};

self.onmessage = async ({ data }) => {
  const { suiteUrl, suiteName, missing, only, shard } = data;
  try {
    const coreModules = [];
    for (const core of [`${suiteName}.core.wasm`, `${suiteName}.core2.wasm`]) {
      const res = await fetch(new URL(core, suiteUrl));
      if (res.ok) coreModules.push(new Uint8Array(await res.arrayBuffer()));
    }
    const tagsOf = inventoryLookup(coreModules);
    const { tests } = await import(new URL(`${suiteName}.js`, suiteUrl));
    const counts = await runCases({
      cases: await tests.all(),
      Context,
      tagsOf,
      missing,
      only,
      shard,
      emit: (event, index) => self.postMessage({ kind: "event", index, event }),
    });
    self.postMessage({ kind: "counts", counts });
  } catch (err) {
    self.postMessage({ kind: "error", error: String(err?.stack ?? err) });
  }
};
