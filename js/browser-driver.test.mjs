// Unit checks for the browser page driver's node-side pieces
// (js/browser-driver.mjs): the harness-page builder, the self-mount
// import map, and the Chrome ladder's env override. The in-page halves
// (page-runner, browser-worker) are syntax-checked here and
// integration-gated by the consumers' browser legs. Plain node, no
// browser. `just verify-imports`.

import assert from "node:assert/strict";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  MOUNT,
  buildHarnessPage,
  componentTestImportMap,
  findChrome,
} from "./browser-driver.mjs";

// The import map points every bare specifier at the self-mount.
const map = componentTestImportMap();
for (const [specifier, path] of Object.entries(map)) {
  assert.ok(specifier.startsWith("@polymorph/component-test-js/"));
  assert.ok(path.startsWith(`${MOUNT}/js/viewer/`));
}

// The page: import map merged with the caller's, config JSON embedded,
// page-runner and worker reached through the mount.
const html = buildHarnessPage({
  importMap: { "my:sut": "/js/sut.js" },
  config: {
    jobs: 1,
    suites: [
      {
        suite: "conformance-guest-ct",
        target: "jco-browser",
        moduleUrl: "/gen/suite.js",
        coreUrls: ["/gen/suite.core.wasm"],
        importsUrl: "/jco/imports.mjs",
      },
    ],
  },
});
assert.ok(html.includes(`"my:sut":"/js/sut.js"`), "caller import-map entry");
assert.ok(html.includes(map["@polymorph/component-test-js/harness"]), "self-mount entry");
assert.ok(html.includes(`${MOUNT}/js/viewer/page-runner.mjs`));
assert.ok(html.includes(`${MOUNT}/js/viewer/browser-worker.mjs`));
assert.ok(html.includes(`"jco-browser"`), "config embedded");

// findChrome: an executable CHROME_PATH wins outright; a missing one
// falls through (to an error here, on a machine-independent HOME).
const dir = await mkdtemp(join(tmpdir(), "browser-driver-test-"));
try {
  const fake = join(dir, "fake-chrome");
  await writeFile(fake, "#!/bin/sh\n");
  await chmod(fake, 0o755);
  assert.equal(await findChrome({ CHROME_PATH: fake, HOME: dir }), fake);
  const sys = await findChrome({ HOME: dir }).catch((e) => e);
  if (sys instanceof Error) {
    assert.match(sys.message, /no Chromium\/Chrome binary found/);
  } else {
    assert.ok(sys.startsWith("/"), "fell through to a system binary");
  }
} finally {
  await rm(dir, { recursive: true, force: true });
}

console.log("browser-driver selftest OK");
