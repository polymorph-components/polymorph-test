//! Sample test suite on the `#[suite]` attribute DX: a passing case, a
//! failing case, and a runtime-skipped case (the exceptional escape
//! hatch), all emitting diagnostics.

use component_test_sdk::prelude::*;

#[component_test_sdk::suite]
mod sample {
    mod math {
        use component_test_sdk::prelude::*;

        #[case]
        async fn add(ctx: &Context) -> Verdict {
            ctx.diagnostic("computing 2 + 2".into()).await;
            let got = 2 + 2;
            ctx.diagnostic(format!("got {got}")).await;
            check_eq!(got, 4, "2 + 2");
            Ok(())
        }

        #[case]
        async fn mul(ctx: &Context) -> Verdict {
            ctx.diagnostic("computing 6 * 9".into()).await;
            let got = 6 * 9;
            ctx.diagnostic(format!("got {got}, expecting the ultimate answer"))
                .await;
            check!(got == 42, "6 * 9: expected 42, got {got}");
            Ok(())
        }
    }

    mod token {
        use component_test_sdk::prelude::*;

        /// Demonstrates the runtime escape hatch: a run-stable target
        /// fact (a hardware token, per the manifest) turns out not to
        /// hold at run time. The case asserts what it can and reports a
        /// claim.
        #[case]
        async fn attest(ctx: &Context) -> Verdict {
            ctx.diagnostic("probing for hardware token".into()).await;
            let token_present = false; // simulated: token unplugged
            if token_present {
                unreachable!("would attest here");
            }
            ctx.diagnostic("token unavailable; asserting clean error".into())
                .await;
            skipped(
                "token unavailable at run time; asserted attestation fails cleanly \
                 (no hang, no partial attestation)",
            )
        }
    }
}
