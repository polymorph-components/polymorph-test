//! Composed wasip3 CLI runner core.
//!
//! Executes every case in the composed suite sequentially, draining each
//! case's diagnostics stream concurrently with its `run`, and reports
//! over wasi:cli stdout — write-through, so output survives a late trap.
//!
//! Two output modes:
//! - human-readable lines (default)
//! - JSONL results events (`COMPONENT_TEST_JSONL=1`): the #26 edge
//!   encoding, this core acting as the generic-host adapter from the
//!   typed results model to stdout.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "runner-cli",
        generate_all,
    });
}

use bindings::exports::wasi::cli::run::Guest as RunGuest;
use bindings::lann::component_test::tests::{all, Outcome, TestCase};
use bindings::lann::component_test_provider::factory::new_context;
use bindings::wasi::cli::stdout::write_via_stream;
use bindings::wit_stream;

use component_test_results::{
    CaseResult, Envelope, Provenance, RunInfo, Status, SuiteInfo, RESULTS_VERSION, TERMINATOR,
};
use futures::{pin_mut, select_biased, FutureExt, StreamExt};
use wit_bindgen::rt::async_support::StreamWriter;

fn jsonl_mode() -> bool {
    std::env::var("COMPONENT_TEST_JSONL").is_ok_and(|v| v == "1")
}

struct Out {
    tx: StreamWriter<u8>,
}

impl Out {
    async fn line(&mut self, s: &str) {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(b'\n');
        let rejected = self.tx.write_all(bytes).await;
        assert!(rejected.is_empty(), "stdout hung up");
    }
}

enum CaseVerdict {
    Pass,
    Fail(String),
    Skip(String),
}

/// Run one case, returning the verdict and its diagnostics (already
/// written through in human mode; collected for the event in JSONL
/// mode).
async fn run_case(case: &TestCase, out: &mut Out, human: bool) -> (CaseVerdict, Vec<String>) {
    let (ctx, observer) = new_context();
    // In-flight read state lives in the stream object, not the `next()`
    // future: dropping a pending `next()` (as `select!` does every
    // iteration) must not lose data already copied into the buffer.
    let mut diag = observer.diagnostics().into_stream();
    let mut diags = Vec::new();

    let verdict = {
        let run_fut = case.run(&ctx).fuse();
        pin_mut!(run_fut);
        let mut verdict = None;
        while verdict.is_none() {
            // Biased, diagnostics first: drain a completed message
            // before taking the verdict, or it dies with the future.
            select_biased! {
                d = diag.next().fuse() => {
                    if let Some(msg) = d {
                        if human {
                            out.line(&format!("    diag: {msg}")).await;
                        }
                        diags.push(msg);
                    }
                }
                r = run_fut => verdict = Some(r),
            }
        }
        verdict.unwrap()
    };

    // Run resolved: drop our end of the context so the stream closes,
    // then drain what's left.
    drop(ctx);
    while let Some(msg) = diag.next().await {
        if human {
            out.line(&format!("    diag: {msg}")).await;
        }
        diags.push(msg);
    }
    drop(observer);

    let verdict = match verdict {
        Ok(()) => CaseVerdict::Pass,
        Err(Outcome::Failed(d)) => CaseVerdict::Fail(d),
        Err(Outcome::Skipped(d)) => CaseVerdict::Skip(d),
    };
    (verdict, diags)
}

/// The typed #26 event, serialized by the shared schema crate —
/// serialization of these plain structs cannot fail.
fn case_event(name: &str, verdict: &CaseVerdict, diags: &[String]) -> String {
    let (status, detail) = match verdict {
        CaseVerdict::Pass => (Status::Pass, None),
        CaseVerdict::Fail(d) => (Status::Fail, Some(d.clone())),
        CaseVerdict::Skip(d) => (Status::Skipped, Some(d.clone())),
    };
    let event = CaseResult {
        case: name.to_string(),
        status,
        provenance: Some(Provenance::Returned),
        detail,
        seed: None,
        duration_ms: None,
        diagnostics: diags.to_vec(),
        diagnostics_complete: true,
    };
    serde_json::to_string(&event).expect("serialize case event")
}

struct Runner;

impl RunGuest for Runner {
    async fn run() -> Result<(), ()> {
        let human = !jsonl_mode();

        // Write-through stdout: a stream we feed as we go.
        let (tx, rx) = wit_stream::new::<u8>();
        let stdout_done = write_via_stream(rx);
        let mut out = Out { tx };

        if !human {
            // This core is generic (any suite gets plugged in via
            // `wac plug`), so it cannot know the suite's name; a
            // neutral one beats the lie of a hardcoded one. No
            // artifact hash either: the composed binary has no access
            // to the suite artifact's bytes.
            let envelope = Envelope {
                version: RESULTS_VERSION.into(),
                target: "composed-cli".into(),
                suite: SuiteInfo {
                    name: "composed".into(),
                    ..Default::default()
                },
                run: RunInfo::default(),
            };
            out.line(&serde_json::to_string(&envelope).expect("serialize envelope"))
                .await;
        }

        let cases = all().await;
        let total = cases.len();
        let (mut passed, mut failed, mut skipped) = (0, 0, 0);

        for case in cases {
            let name = case.name();
            if human {
                out.line(&format!("test {name} ...")).await;
            }

            let (verdict, diags) = run_case(&case, &mut out, human).await;

            if human {
                match &verdict {
                    CaseVerdict::Pass => out.line(&format!("test {name}: PASS")).await,
                    CaseVerdict::Fail(d) => out.line(&format!("test {name}: FAIL: {d}")).await,
                    CaseVerdict::Skip(d) => out.line(&format!("test {name}: SKIP: {d}")).await,
                }
            } else {
                out.line(&case_event(&name, &verdict, &diags)).await;
            }

            match verdict {
                CaseVerdict::Pass => passed += 1,
                CaseVerdict::Fail(_) => failed += 1,
                CaseVerdict::Skip(_) => skipped += 1,
            }
        }

        if human {
            out.line(&format!(
                "\nresult: {passed} passed, {failed} failed, {skipped} skipped, {total} total"
            ))
            .await;
        } else {
            out.line(TERMINATOR).await;
        }

        // Close stdout stream and wait for delivery.
        drop(out);
        let _ = stdout_done.await;

        // Empty selection is a run error (normative fold rule): a
        // suite whose cases were all compiled away must not report
        // vacuous success.
        if failed == 0 && total > 0 {
            Ok(())
        } else {
            Err(())
        }
    }
}

bindings::export!(Runner with_types_in bindings);
