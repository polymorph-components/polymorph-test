//! Synthetic benchmark suite: N trivially-passing cases, no corpus, no
//! per-case data — the pure cost of registering, minting, and lifting
//! `test-case` handles. Built for measuring `all()` overhead at scale
//! (issues #22/#25 territory: instance-per-case pays this per instance).
//!
//! `BENCH_CASES` (wasi:cli env) sets the case count, default 10_000.
//! Bench drivers vary it per instance; under a plain runner (which
//! passes no env) the suite is deterministic at the default, but this
//! is a measurement fixture, not a lockfile citizen — don't lock it.

#[component_test_sdk::suite]
pub mod bench {
    use component_test_sdk::{ArcStr, Registry, Tags};

    /// Registers `bench/mint/c00000`..`c<N-1>`: leaf-only allocation,
    /// zero-capture bodies, no tags. The registry build cost this loop
    /// represents is measured separately from the mint+lift (the
    /// drivers time a second `all()` call, which reuses the registry).
    #[case_row(prefix = "mint")]
    fn mint(reg: &mut Registry, prefix: &ArcStr, tags: &Tags) {
        let n: usize = std::env::var("BENCH_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10_000);
        for i in 0..n {
            reg.generated(
                prefix,
                tags,
                Case::new(format!("c{i:05}"), |_ctx| Box::pin(async { Ok(()) })),
            );
        }
    }

    /// Force the registry build (pre-initialization hook; see
    /// `wizer_init` below). Always compiled — `#[suite]` forbids
    /// cfg-gated items inside the module tree.
    pub fn force_registry_init() {
        __ct_with_registry(|_| ());
    }
}

/// Pre-initialization entry for the wizer experiment (#25): a second,
/// bare-named world export (`wizer-initialize`, wizer's default init
/// function) that forces the registry build so the snapshot carries it.
/// Feature-gated and outside the `#[suite]` module — the plain bench
/// artifact keeps the pure `suite` world. The versioned `tests`
/// interface itself cannot be named as an init function (wasmtime's
/// invoke grammar rejects `@0.1.0` qualifiers), hence the extra export.
#[cfg(feature = "wizer-init")]
mod wizer_init {
    wit_bindgen::generate!({
        inline: "
            package bench:wizer;
            world init {
                export wizer-initialize: func();
            }
        ",
        world: "init",
    });
    struct Init;
    impl Guest for Init {
        fn wizer_initialize() {
            crate::bench::force_registry_init();
        }
    }
    export!(Init);
}
