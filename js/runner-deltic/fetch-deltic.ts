// Fetch (and cache) deltic release assets for the pinned release.
//
// deltic is a runtime linker: components are translated by a wasm build of
// its translator, and browser/Node consumers load the runtime as one
// platform-neutral ES module (`deltic-embedder.mjs`) — both shipped as
// release assets so consumers need no Rust or Deno toolchain. This script
// downloads one asset into target/deltic/<tag>/, verifies it against the
// pinned sha256, and prints the cached path on stdout (the `verify-deltic`
// recipe captures it).
//
//   deno run … fetch-deltic.ts --asset translator   # deltic-translator-shim.wasm
//   deno run … fetch-deltic.ts --asset embedder     # deltic-embedder.mjs
//
// THE PIN lives here (TAG + per-asset sha256) and in the sibling
// import-map URLs (deno.json). `assertPinConsistency` fails loud if the
// two drift. Bumping: update TAG here and in deno.json, update the shas
// from the release's SHA256SUMS, delete deno.lock, and re-run
// `deno cache runner.ts fetch-deltic.ts` in this directory to regenerate
// it (commit the diff).

const TAG = "pre-83fff30";
const ASSETS: Record<string, { file: string; sha256: string }> = {
  translator: {
    file: "deltic-translator-shim.wasm",
    sha256: "6d02b363785593595a789d083cda0aebb1de790726718ccf543198354fa3870c",
  },
  embedder: {
    file: "deltic-embedder.mjs",
    sha256: "b9ceb33c78abdaa4311f681c1388b14a3471f8a17ebd2dbf2dddd4a596df72c3",
  },
};

const HERE = new URL(".", import.meta.url);
const REPO_ROOT = new URL("../../", HERE);

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    bytes as BufferSource,
  );
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/** The one-pin-everywhere gate: every raw.githubusercontent URL in the
 * sibling import map must reference TAG. */
async function assertPinConsistency(): Promise<void> {
  const denoJson = await Deno.readTextFile(new URL("deno.json", HERE));
  const urls = denoJson.match(/https:\/\/raw\.githubusercontent\.com[^"]+/g) ?? [];
  if (urls.length === 0) {
    throw new Error("deno.json: no pinned deltic URLs found");
  }
  for (const url of urls) {
    if (!url.includes(`/lann/deltic/${TAG}/`)) {
      throw new Error(
        `pin drift: deno.json pins ${url}\nbut fetch-deltic.ts pins ${TAG}`,
      );
    }
  }
}

async function main() {
  const idx = Deno.args.indexOf("--asset");
  const which = idx >= 0 ? Deno.args[idx + 1] : undefined;
  const asset = which !== undefined ? ASSETS[which] : undefined;
  if (asset === undefined) {
    console.error(
      `usage: fetch-deltic.ts --asset <${Object.keys(ASSETS).join("|")}>`,
    );
    Deno.exit(2);
  }

  await assertPinConsistency();

  const cacheDir = new URL(`target/deltic/${TAG}/`, REPO_ROOT);
  const cached = new URL(asset.file, cacheDir);
  try {
    const bytes = await Deno.readFile(cached);
    if (await sha256Hex(bytes) === asset.sha256) {
      console.log(cached.pathname);
      return;
    }
    console.error(`cached ${asset.file} has a stale digest; re-fetching`);
  } catch {
    // not cached yet
  }

  const releaseUrl =
    `https://github.com/lann/deltic/releases/download/${TAG}/${asset.file}`;
  console.error(`fetching ${releaseUrl} …`);
  const resp = await fetch(releaseUrl);
  if (!resp.ok) {
    throw new Error(`GET ${releaseUrl}: ${resp.status} ${resp.statusText}`);
  }
  const bytes = new Uint8Array(await resp.arrayBuffer());
  const got = await sha256Hex(bytes);
  if (got !== asset.sha256) {
    throw new Error(
      `sha256 mismatch for ${asset.file}@${TAG}:\n  want ${asset.sha256}\n  got  ${got}`,
    );
  }
  await Deno.mkdir(cacheDir, { recursive: true });
  await Deno.writeFile(cached, bytes);
  console.log(cached.pathname);
}

await main();
