#!/usr/bin/env node
// Static server for the viewer: serves the repository root (so the
// demo can fetch committed fixtures and transpiled suites by relative
// path) with the mime types wasm + ES modules need. No dependencies.
//
// Usage: node serve.mjs [port]   (or `just viewer-serve`)
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const PORT = Number(process.argv[2] ?? 8123);

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".mjs": "text/javascript",
  ".css": "text/css",
  ".json": "application/json",
  ".jsonl": "application/jsonl",
  ".wasm": "application/wasm",
  ".toml": "text/plain; charset=utf-8",
  ".lock": "text/plain; charset=utf-8",
  ".md": "text/plain; charset=utf-8",
  ".ts": "text/plain; charset=utf-8",
};

createServer(async (req, res) => {
  const path = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname));
  const file = join(ROOT, path === "/" ? "/js/viewer/index.html" : path);
  if (!file.startsWith(ROOT)) {
    res.writeHead(403).end();
    return;
  }
  try {
    const body = await readFile(file);
    res.writeHead(200, { "content-type": MIME[extname(file)] ?? "application/octet-stream" });
    res.end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
}).listen(PORT, "127.0.0.1", () => {
  console.log(`viewer: http://127.0.0.1:${PORT}/js/viewer/index.html (serving repo root)`);
});
