// The deltic runner leg (Path 3b): drive an L1 suite under deltic — a
// runtime linker, so there is NO transpile step and NO engine flag; the
// contract's async exports run on the callback ABI under stock Deno.
//
// Mirrors runner-node/runner-host-provider.mjs's topology (runner-is-
// provider: the host supplies test-context) and, in human mode, its exact
// output format — `expected/verify-run-sample.txt` is shared verbatim with
// the composed-cli and jco-node legs. With --jsonl it emits canonical L4
// results JSONL instead (deltic's runSuite mirrors js/viewer/harness.mjs
// semantics; schema authority: crates/component-test-results).
//
//   deno run --allow-read=target --config js/runner-deltic/deno.json \
//     --frozen js/runner-deltic/runner.ts <suite.wasm> \
//     [--translator <translator_shim.wasm>] [--jsonl] [--target NAME]
//
// The translator comes from the pinned @deltic/translator package through
// the module graph (permission-free: no net grant, no read grant for the
// asset). `--translator` stays as a documented escape hatch for driving an
// externally-sourced translator wasm; absent, the packaged one is used.
//
// The deltic pin lives in deno.json + deno.lock (which see, and README.md
// for the bump procedure).

import { Translator } from "@deltic/runtime/shim";
import { defaultTranslator } from "@deltic/translator";
import { runSuite } from "@deltic/ct-runner";
import { wasiShims } from "@deltic/wasi-shims";

interface Cli {
  suitePath: string;
  translator?: string;
  jsonl: boolean;
  target: string;
  missing?: string[];
}

function parseArgs(argv: string[]): Cli {
  const positional: string[] = [];
  let translator: string | undefined;
  let jsonl = false;
  let target = "deltic/deno";
  let missing: string[] | undefined;
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    switch (a) {
      case "--translator":
        translator = argv[++i];
        break;
      case "--jsonl":
        jsonl = true;
        break;
      case "--target":
        target = argv[++i];
        break;
      case "--missing":
        missing = argv[++i].split(",").filter((f) => f !== "");
        break;
      default:
        if (a.startsWith("--")) throw new Error(`unknown flag '${a}'`);
        positional.push(a);
    }
  }
  if (positional.length !== 1) {
    console.error(
      "usage: runner.ts <suite.wasm> [--translator <translator_shim.wasm>] " +
        "[--jsonl] [--target NAME] [--missing f1,f2,...]",
    );
    Deno.exit(2);
  }
  return { suitePath: positional[0], translator, jsonl, target, missing };
}

interface CaseEvent {
  case: string;
  status: string;
  detail?: string;
  diagnostics?: string[];
}

/** Render one case event in the shared human format (the byte-exact
 * contract of expected/verify-run-sample.txt, established by
 * components/runner-cli and runner-node/runner-host-provider.mjs). */
function renderHuman(e: CaseEvent): string {
  const lines = [`test ${e.case} ...`];
  for (const d of e.diagnostics ?? []) lines.push(`    diag: ${d}`);
  switch (e.status) {
    case "pass":
      lines.push(`test ${e.case}: PASS`);
      break;
    case "fail":
      lines.push(`test ${e.case}: FAIL: ${e.detail ?? ""}`);
      break;
    case "skipped":
      lines.push(`test ${e.case}: SKIP: ${e.detail ?? ""}`);
      break;
    case "not-applicable":
      // Tag scheduling (deltic ct-runner #25): the case was scheduled out
      // for this target, not executed. No shared human golden constrains
      // this line (the composed/jco legs never see tags).
      lines.push(`test ${e.case}: N/A: ${e.detail ?? ""}`);
      break;
    default:
      throw new Error(`unknown case status '${e.status}' (schema drift?)`);
  }
  return lines.join("\n");
}

function suiteNameFrom(path: string): string {
  const base = path.split("/").pop() ?? path;
  return base.replace(/\.component\.wasm$|\.wasm$/, "");
}

async function main() {
  const cli = parseArgs(Deno.args);
  const componentBytes = await Deno.readFile(cli.suitePath);
  // Packaged by default (loads through the module graph, no permissions);
  // --translator drives an externally-sourced translator wasm instead.
  const translator = cli.translator === undefined
    ? await defaultTranslator()
    : await Translator.create(await Deno.readFile(cli.translator));
  const { plan, adapters } = translator.translate(componentBytes);

  const lines: string[] = [];
  const counts = await runSuite({ plan, componentBytes, adapters }, {
    imports: wasiShims(),
    target: cli.target,
    suiteName: suiteNameFrom(cli.suitePath),
    missing: cli.missing,
    emit: (line: string) => lines.push(line),
  });

  if (cli.jsonl) {
    console.log(lines.join("\n"));
  } else {
    for (const line of lines.slice(1, -1)) {
      console.log(renderHuman(JSON.parse(line) as CaseEvent));
    }
    // The result line stays byte-exact with the shared golden when nothing
    // was scheduled out (expected/verify-run-sample.txt); the n/a clause
    // appears only for tag-scheduled runs, which have no shared golden.
    const na = counts.na > 0 ? `, ${counts.na} n/a` : "";
    console.log(
      `\nresult: ${counts.passed} passed, ${counts.failed} failed, ` +
        `${counts.skipped} skipped${na}, ${counts.total} total`,
    );
  }

  // Empty selection is a run error: zero cases must not exit green
  // (runner-host-provider.mjs's exit discipline, shared by every leg).
  Deno.exit(counts.failed === 0 && counts.total > 0 ? 0 : 1);
}

await main();
