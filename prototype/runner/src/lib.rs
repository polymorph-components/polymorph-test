//! Prototype wasip3 CLI runner core.
//!
//! Executes every case in the composed suite sequentially, draining each
//! case's diagnostics stream concurrently with its `run`, and reports
//! over wasi:cli stdout (write-through, so output survives a late trap).

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "runner-cli",
        generate_all,
    });
}

use bindings::exports::wasi::cli::run::Guest as RunGuest;
use bindings::lann::component_test::tests::{all, Outcome};
use bindings::lann::component_test_provider::factory::new_context;
use bindings::wasi::cli::stdout::write_via_stream;
use bindings::wit_stream;

use futures::{pin_mut, select_biased, FutureExt, StreamExt};
use wit_bindgen::rt::async_support::StreamWriter;

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

struct Runner;

impl RunGuest for Runner {
    async fn run() -> Result<(), ()> {
        // Write-through stdout: a stream we feed as we go.
        let (tx, rx) = wit_stream::new::<u8>();
        let stdout_done = write_via_stream(rx);
        let mut out = Out { tx };

        let cases = all().await;
        let total = cases.len();
        let mut passed = 0;
        let mut failed = 0;
        let mut skipped = 0;

        for case in cases {
            let name = case.name();
            out.line(&format!("test {name} ...")).await;

            let (ctx, observer) = new_context();
            // `into_stream` keeps in-flight read state in the stream
            // object rather than the `next()` future: dropping a pending
            // `next()` (as `select!` does every iteration) must not lose
            // data that was already copied into the read buffer.
            let mut diag = observer.diagnostics().into_stream();

            // Drain diagnostics concurrently with the run.
            let verdict = {
                let run_fut = case.run(&ctx).fuse();
                pin_mut!(run_fut);
                let mut verdict = None;
                while verdict.is_none() {
                    // Biased, diagnostics first: if a message completed in
                    // the same wake-up as the verdict, drain it before
                    // taking the verdict — dropping a completed `next()`
                    // future would lose the message it already consumed.
                    select_biased! {
                        d = diag.next().fuse() => {
                            if let Some(msg) = d {
                                out.line(&format!("    diag: {msg}")).await;
                            }
                        }
                        r = run_fut => verdict = Some(r),
                    }
                }
                verdict.unwrap()
            };

            // Run resolved: drop our end of the context so the stream
            // closes, then drain whatever is left.
            drop(ctx);
            while let Some(msg) = diag.next().await {
                out.line(&format!("    diag: {msg}")).await;
            }
            drop(observer);

            match verdict {
                Ok(()) => {
                    passed += 1;
                    out.line(&format!("test {name}: PASS")).await;
                }
                Err(Outcome::Failed(reason)) => {
                    failed += 1;
                    out.line(&format!("test {name}: FAIL: {reason}")).await;
                }
                Err(Outcome::Skipped(claim)) => {
                    skipped += 1;
                    out.line(&format!("test {name}: SKIP: {claim}")).await;
                }
            }
        }

        out.line(&format!(
            "\nresult: {passed} passed, {failed} failed, {skipped} skipped, {total} total"
        ))
        .await;

        // Close stdout stream and wait for delivery.
        drop(out);
        let _ = stdout_done.await;

        if failed == 0 {
            Ok(())
        } else {
            Err(())
        }
    }
}

bindings::export!(Runner with_types_in bindings);
