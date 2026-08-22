// The viewer application: two panes over one data model.
//
// Results pane: lockfile + manifest + per-target results-JSONL streams
// go through the viewer-aggregate component — the gate's own Rust
// aggregation compiled to wasm — and the returned document renders as
// the matrix, the validation surface (errors/warnings, never hidden),
// and a per-case detail drawer.
//
// Live pane: a transpiled suite runs in a Web Worker pool (worker.mjs
// over the shared harness.mjs, striped like every other runner), rows
// stream into a live table, and the finished run downloads as
// results-JSONL or feeds straight into the Results pane.
import { envelope, mergeCounts, workerCount } from "./harness.mjs";
import { aggregateEngine, bundleUrl, translatorUrl } from "./polyengine.mjs";

const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------- tabs
function showPane(which) {
  $("pane-results").hidden = which !== "results";
  $("pane-live").hidden = which !== "live";
  $("tab-results").classList.toggle("active", which === "results");
  $("tab-live").classList.toggle("active", which === "live");
}
$("tab-results").onclick = () => showPane("results");
$("tab-live").onclick = () => showPane("live");

// ------------------------------------------------------- results state
const state = {
  lock: null, // string
  manifest: null, // string
  streams: [], // [{ target, text, source }]
};

function envelopeTarget(text) {
  try {
    return JSON.parse(text.slice(0, text.indexOf("\n"))).target ?? null;
  } catch {
    return null;
  }
}

function refreshInputs() {
  $("lock-loaded").textContent = state.lock ? "✓" : "";
  $("manifest-loaded").textContent = state.manifest ? "✓" : "";
  $("results-loaded").textContent = state.streams.length
    ? state.streams.map((s) => s.target).join(", ")
    : "";
  $("btn-aggregate").disabled = !(state.lock && state.manifest && state.streams.length);
}

$("in-lock").onchange = async (e) => {
  state.lock = await e.target.files[0]?.text();
  refreshInputs();
};
$("in-manifest").onchange = async (e) => {
  state.manifest = await e.target.files[0]?.text();
  refreshInputs();
};
$("in-results").onchange = async (e) => {
  state.streams = [];
  for (const f of e.target.files) {
    const text = await f.text();
    const target = envelopeTarget(text) ?? f.name.replace(/\.[^.]*$/, "");
    state.streams.push({ target, text, source: f.name });
  }
  refreshInputs();
};

$("btn-demo").onclick = async () => {
  // The fixture-suite walkthrough, served from the repository tree.
  const get = async (path) => {
    const res = await fetch(path);
    if (!res.ok) throw new Error(`${path}: ${res.status} (serve the page from the repo root: just viewer-serve)`);
    return res.text();
  };
  try {
    state.lock = await get("../../components/fixture-suite/tests.lock");
    state.manifest = await get("../../examples/aggregate/targets.toml");
    const stream = await get("../../expected/verify-pipeline-fixture.jsonl");
    // The committed stream is a `--missing hsm` run, i.e. the manifest's
    // `sim` target. Loading it alone leaves `native` reporting nothing —
    // deliberately: the error banner below is the point of this page.
    // The Live tab's defaults produce exactly the missing leg (run the
    // fixture suite as `native`, hsm present), and "Send to Results"
    // completes the matrix.
    state.streams = [{ target: "sim", text: stream, source: "expected/verify-pipeline-fixture.jsonl" }];
    refreshInputs();
    renderAggregate();
  } catch (err) {
    alert(String(err?.message ?? err));
  }
};

$("btn-aggregate").onclick = renderAggregate;

async function renderAggregate() {
  let doc;
  try {
    const aggregate = await aggregateEngine();
    doc = JSON.parse(
      await aggregate(state.lock, state.manifest, state.streams.map((s) => [s.target, s.text])),
    );
  } catch (err) {
    // Parse failures of the inputs themselves (not validation findings).
    renderFindings({ errors: [String(err?.payload ?? err?.message ?? err)], warnings: [] });
    $("verdict").hidden = true;
    $("matrix").hidden = true;
    return;
  }
  renderVerdict(doc);
  renderFindings(doc);
  renderMatrix(doc);
}

// ------------------------------------------------------------- verdict
function renderVerdict(doc) {
  const s = doc.summary;
  const el = $("verdict");
  el.hidden = false;
  el.innerHTML = "";
  const badge = document.createElement("span");
  badge.className = `badge ${s.ok ? "ok" : "bad"}`;
  badge.textContent = s.ok ? "OK" : "FAILING";
  el.append(badge);
  const stats = [
    ["targets", s.targets],
    ["results", s.results],
    ["failing", s.failing],
    ["expected-fail", s["expected-fail"]],
    ["errors", doc.errors.length],
    ["warnings", doc.warnings.length],
  ];
  for (const [k, v] of stats) {
    const span = document.createElement("span");
    span.className = "stat";
    span.innerHTML = `<b>${v}</b> ${k}`;
    el.append(span);
  }
}

function renderFindings(doc) {
  const el = $("findings");
  el.innerHTML = "";
  el.hidden = !doc.errors.length && !doc.warnings.length;
  for (const [kind, items] of [["error", doc.errors], ["warning", doc.warnings]]) {
    if (!items.length) continue;
    const div = document.createElement("div");
    div.className = `finding ${kind}`;
    const label = document.createElement("b");
    label.textContent = `${items.length} ${kind}${items.length > 1 ? "s" : ""}`;
    const ul = document.createElement("ul");
    for (const item of items) {
      const li = document.createElement("li");
      li.textContent = item;
      ul.append(li);
    }
    div.append(label, ul);
    el.append(div);
  }
}

// -------------------------------------------------------------- matrix
const STATUS_GLYPH = {
  pass: "pass",
  fail: "FAIL",
  skipped: "skip",
  "not-applicable": "N/A",
  "not-reached": "NOT-REACHED",
};

function statusClass(target, name, r, doc) {
  const assess = doc.assessments?.[target]?.[name];
  if (assess?.kind === "expected-fail" && r.status === "fail") return "s-xfail";
  if (assess?.kind === "unexpected-pass") return "s-upass";
  return `s-${r.status}`;
}

function statusText(target, name, r, doc) {
  const assess = doc.assessments?.[target]?.[name];
  if (assess?.kind === "expected-fail" && r.status === "fail") return "XFAIL";
  if (assess?.kind === "unexpected-pass") return "UNEXPECTED-PASS";
  return STATUS_GLYPH[r.status] ?? r.status;
}

function groupOf(name) {
  return name.split("/")[0];
}

function renderMatrix(doc) {
  const el = $("matrix");
  el.hidden = false;
  el.innerHTML = "";

  // Union of case names, sorted; grouped by first segment.
  const cases = new Set();
  for (const t of doc.targets) {
    for (const name of Object.keys(doc.results[t] ?? {})) cases.add(name);
  }
  const groups = new Map();
  for (const name of [...cases].sort()) {
    const g = groupOf(name);
    if (!groups.has(g)) groups.set(g, []);
    groups.get(g).push(name);
  }

  const controls = document.createElement("div");
  controls.className = "controls";
  const search = document.createElement("input");
  search.type = "search";
  search.placeholder = "filter cases (substring)";
  const failingOnly = document.createElement("label");
  const cb = document.createElement("input");
  cb.type = "checkbox";
  failingOnly.append(cb, "problems only (failing, unexpected-pass, not-reached)");
  controls.append(search, failingOnly);
  el.append(controls);

  const table = document.createElement("table");
  const thead = document.createElement("thead");
  const hr = document.createElement("tr");
  hr.append(document.createElement("th"));
  hr.firstChild.textContent = `case (${cases.size})`;
  for (const t of doc.targets) {
    const th = document.createElement("th");
    th.textContent = t;
    hr.append(th);
  }
  thead.append(hr);
  table.append(thead);
  const tbody = document.createElement("tbody");
  table.append(tbody);
  el.append(table);

  const problem = (name) =>
    doc.targets.some((t) => {
      const r = doc.results[t]?.[name];
      if (!r) return false;
      return r.failing || doc.assessments?.[t]?.[name]?.kind === "unexpected-pass" ||
        r.status === "not-reached";
    });

  const open = new Set();
  function rebuild() {
    tbody.innerHTML = "";
    const q = search.value.trim();
    for (const [g, names] of groups) {
      const visible = names.filter(
        (n) => (!q || n.includes(q)) && (!cb.checked || problem(n)),
      );
      if (!visible.length) continue;
      const gr = document.createElement("tr");
      gr.className = "group" + (open.has(g) ? " open" : "");
      const gtd = document.createElement("td");
      gtd.textContent = `${g} (${visible.length} case${visible.length > 1 ? "s" : ""})`;
      gr.append(gtd);
      for (const t of doc.targets) {
        const td = document.createElement("td");
        td.className = "cell";
        const counts = {};
        for (const n of visible) {
          const r = doc.results[t]?.[n];
          const label = r ? statusText(t, n, r, doc) : "—";
          counts[label] = (counts[label] ?? 0) + 1;
        }
        const parts = Object.entries(counts).map(([label, c]) => {
          const cls =
            label === "pass" ? "s-pass"
            : label === "FAIL" || label === "UNEXPECTED-PASS" || label === "NOT-REACHED" ? "s-fail"
            : label === "XFAIL" ? "s-xfail"
            : label === "skip" ? "s-skipped"
            : "s-na";
          return `<span class="${cls}">${c === visible.length ? label : `${c} ${label}`}</span>`;
        });
        td.innerHTML = parts.join(", ");
        gr.append(td);
      }
      gtd.onclick = () => {
        open.has(g) ? open.delete(g) : open.add(g);
        rebuild();
      };
      tbody.append(gr);
      if (!open.has(g)) continue;
      for (const n of visible) {
        const tr = document.createElement("tr");
        tr.className = "case";
        const td = document.createElement("td");
        td.textContent = n;
        tr.append(td);
        for (const t of doc.targets) {
          const cell = document.createElement("td");
          const r = doc.results[t]?.[n];
          if (r) {
            cell.className = `cell ${statusClass(t, n, r, doc)}`;
            cell.textContent = statusText(t, n, r, doc);
          } else {
            cell.className = "cell s-na";
            cell.textContent = "—";
          }
          tr.append(cell);
        }
        tr.onclick = () => renderDetail(doc, n);
        tbody.append(tr);
      }
    }
  }
  search.oninput = rebuild;
  cb.onchange = rebuild;
  rebuild();
}

function renderDetail(doc, name) {
  const el = $("detail");
  el.hidden = false;
  el.innerHTML = "";
  const close = document.createElement("button");
  close.className = "close";
  close.textContent = "✕";
  close.onclick = () => (el.hidden = true);
  const h = document.createElement("h2");
  h.textContent = name;
  el.append(close, h);
  for (const t of doc.targets) {
    const r = doc.results[t]?.[name];
    const dl = document.createElement("dl");
    const dt = document.createElement("dt");
    dt.textContent = t;
    dl.append(dt);
    const dd = document.createElement("dd");
    if (!r) {
      dd.textContent = "no result";
      dd.className = "s-na";
      dl.append(dd);
      el.append(dl);
      continue;
    }
    dd.className = statusClass(t, name, r, doc);
    dd.textContent = statusText(t, name, r, doc) +
      (r.provenance ? ` (${r.provenance})` : "");
    dl.append(dd);
    const assess = doc.assessments?.[t]?.[name];
    if (assess?.kind === "expected-fail") {
      const a = document.createElement("dd");
      a.className = "assess";
      a.textContent = `expected-fail: ${assess.reason} — ${assess.tracking}`;
      dl.append(a);
    }
    if (r.detail) {
      const pre = document.createElement("pre");
      pre.textContent = r.detail;
      dl.append(pre);
    }
    if (r.diagnostics?.length) {
      const dt2 = document.createElement("dt");
      dt2.textContent = `diagnostics${r["diagnostics-complete"] === false ? " (incomplete)" : ""}`;
      const pre = document.createElement("pre");
      pre.textContent = r.diagnostics.join("\n");
      dl.append(dt2, pre);
    }
    el.append(dl);
  }
}

// ------------------------------------------------------------ live run
let liveRows = null; // last finished run: [{index, event}], sorted
let liveMeta = null; // { target, suiteName }

$("btn-run").onclick = async () => {
  const suiteName = $("live-name").value.trim();
  // Directory URLs get the component's file name appended (cargo names
  // it with underscores); a direct .wasm URL is used as-is.
  const raw = $("live-url").value;
  const suiteUrl = new URL(
    raw.endsWith(".wasm") ? raw : `${raw.replace(/\/?$/, "/")}${suiteName.replaceAll("-", "_")}.wasm`,
    location.href,
  ).href;
  const missing = $("live-missing").value.split(",").map((s) => s.trim()).filter(Boolean);
  const jobs = workerCount(Number($("live-workers").value) || 1);
  const target = $("live-target").value.trim() || "native";

  $("btn-run").disabled = true;
  $("btn-download").disabled = true;
  $("btn-adopt").disabled = true;
  const verdict = $("live-verdict");
  verdict.hidden = false;
  verdict.textContent = `running ${suiteName} across ${jobs} worker${jobs > 1 ? "s" : ""}…`;

  const tableEl = $("live-table");
  tableEl.innerHTML = "";
  const table = document.createElement("table");
  table.innerHTML = "<thead><tr><th>case</th><th>status</th><th>detail</th></tr></thead>";
  const tbody = document.createElement("tbody");
  table.append(tbody);
  tableEl.append(table);

  const rows = [];
  const live = { passed: 0, failed: 0, skipped: 0, na: 0 };
  const onEvent = (index, event) => {
    rows.push({ index, event });
    if (event.status === "pass") live.passed++;
    else if (event.status === "fail") live.failed++;
    else if (event.status === "skipped") live.skipped++;
    else if (event.status === "not-applicable") live.na++;
    verdict.innerHTML =
      `<span class="stat"><b>${rows.length}</b> run</span>` +
      `<span class="stat s-pass"><b>${live.passed}</b> pass</span>` +
      `<span class="stat s-fail"><b>${live.failed}</b> fail</span>` +
      `<span class="stat s-skipped"><b>${live.skipped}</b> skip</span>` +
      `<span class="stat s-na"><b>${live.na}</b> N/A</span>`;
    // The table shows problems as they happen; full data rides the
    // JSONL download (and the Results pane).
    if (event.status === "fail") {
      const tr = document.createElement("tr");
      tr.innerHTML =
        `<td>${event.case}</td>` +
        `<td class="s-fail">${event.provenance === "trap" ? "TRAP" : "FAIL"}</td>` +
        `<td>${event.detail ?? ""}</td>`;
      tbody.append(tr);
    }
  };

  try {
    const parts = await Promise.all(
      Array.from({ length: jobs }, (_, index) =>
        new Promise((resolve, reject) => {
          const worker = new Worker(
            new URL("../runner-polyengine/browser-worker.mjs", import.meta.url),
            { type: "module" },
          );
          worker.onmessage = ({ data }) => {
            if (data.kind === "event") onEvent(data.index, data.event);
            else if (data.kind === "counts") {
              worker.terminate();
              resolve(data.counts);
            } else {
              worker.terminate();
              reject(new Error(`worker (shard ${index}): ${data.error}`));
            }
          };
          worker.onerror = (e) => {
            worker.terminate();
            reject(new Error(`worker (shard ${index}): ${e.message ?? e}`));
          };
          worker.postMessage({
            bundleUrl,
            translatorUrl,
            suiteUrl,
            missing,
            shard: { index, count: jobs },
          });
        })
      ),
    );
    const counts = mergeCounts(parts);
    rows.sort((a, b) => a.index - b.index);
    liveRows = rows;
    liveMeta = { target, suiteName };
    verdict.innerHTML =
      `<span class="badge ${counts.failed === 0 ? "ok" : "bad"}">${counts.failed === 0 ? "OK" : "FAILING"}</span>` +
      `<span class="stat"><b>${counts.passed}</b> passed, <b>${counts.failed}</b> failed, ` +
      `<b>${counts.skipped}</b> skipped, <b>${counts.na}</b> not applicable, ` +
      `<b>${counts.total}</b> total</span>` +
      `<span class="stat">raw stream verdicts — expected-fail assessment happens ` +
      `in the Results pane, where the manifest is</span>`;
    $("btn-download").disabled = false;
    $("btn-adopt").disabled = false;
  } catch (err) {
    verdict.innerHTML = `<span class="badge bad">RUN ERROR</span><span class="stat">${err?.message ?? err}</span>`;
  } finally {
    $("btn-run").disabled = false;
  }
};

function liveJsonl() {
  const lines = [JSON.stringify(envelope(liveMeta.target, liveMeta.suiteName.replaceAll("-", "_")))];
  for (const { event } of liveRows) lines.push(JSON.stringify(event));
  lines.push('{"segment-end":true}');
  return lines.join("\n") + "\n";
}

$("btn-download").onclick = () => {
  const blob = new Blob([liveJsonl()], { type: "application/jsonl" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = `${liveMeta.target}.jsonl`;
  a.click();
  URL.revokeObjectURL(a.href);
};

$("btn-adopt").onclick = () => {
  const text = liveJsonl();
  const existing = state.streams.filter((s) => s.target !== liveMeta.target);
  state.streams = [...existing, { target: liveMeta.target, text, source: "live run" }];
  refreshInputs();
  showPane("results");
  if (state.lock && state.manifest) renderAggregate();
};
