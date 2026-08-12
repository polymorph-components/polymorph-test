//! Builds the components baked into the CLI — `components/runner-cli`
//! and `components/provider`, the `compose-runner`/`run` defaults —
//! from source at compile time (feature `embedded-components`,
//! default), so the embedded bytes can never go stale against their
//! sources (#88; previously they were committed artifacts refreshed by
//! hand, gated only through sample-suite-visible behavior).
//!
//! Mechanics, each load-bearing:
//!
//! - **Nested cargo, separate target dir**: the inner build writes to
//!   `$OUT_DIR/embed-target` — sharing the outer target dir would
//!   deadlock on cargo's build-dir lock. The inner graph is small
//!   (wit-bindgen runtime, serde_json, the results crate) and cargo's
//!   own fingerprinting caches it there across rebuilds.
//! - **Broad rerun-if-changed** (whole `components/`, `crates/`,
//!   `wit/` trees): over-firing costs a sub-second inner no-op, while
//!   a curated file list would silently go stale when the dependency
//!   closure grows — the exact failure mode this build script exists
//!   to delete. The inner cargo rebuilds precisely what changed.
//! - **Curated environment**: the outer build's host-targeted
//!   compilation vars (`RUSTFLAGS`, `RUSTC`, cargo's own `CARGO_*`
//!   bookkeeping) must not leak into the wasm build. `$CARGO` is used
//!   as the binary (the outer build's resolved toolchain, bypassing
//!   rustup's cwd-based resolution); `CARGO_HOME` and network knobs
//!   survive so registry caches and offline/vendored setups keep
//!   working.
//! - `--locked --profile embed`: the workspace lockfile governs the
//!   inner graph too, and the components ship size-optimized
//!   regardless of the outer profile.
//!
//! Requires the `wasm32-wasip2` target (rust-toolchain.toml carries it
//! in-repo; consumers of this stack have it for their own suites). A
//! host-only CLI — reporting/aggregation consumers — builds with
//! `--no-default-features`; `compose-runner`/`run` then require
//! explicit `--runner`/`--provider`.
//!
//! When cargo's artifact-dependencies (`bindeps`) stabilize, this
//! whole script becomes two `[build-dependencies]` entries; it is the
//! stable-Rust approximation of exactly that.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/component-test-cli sits two levels under the workspace root")
        .to_path_buf();

    for input in ["components", "crates", "wit", "Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", workspace.join(input).display());
    }

    if env::var_os("CARGO_FEATURE_EMBEDDED_COMPONENTS").is_none() {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target_dir = out_dir.join("embed-target");
    let cargo = env::var_os("CARGO").expect("cargo sets $CARGO for build scripts");

    let mut cmd = Command::new(cargo);
    cmd.current_dir(&workspace)
        .args([
            "build",
            "--locked",
            "--profile",
            "embed",
            "--target",
            "wasm32-wasip2",
            "-p",
            "runner-cli",
            "-p",
            "provider",
        ])
        .arg("--target-dir")
        .arg(&target_dir);
    for (key, _) in env::vars_os() {
        let k = key.to_string_lossy().into_owned();
        if !keep_env(&k) {
            cmd.env_remove(&key);
        }
    }

    let status = cmd.status().expect("spawning the inner cargo build");
    if !status.success() {
        eprintln!(
            "\nbuilding the embedded components (components/runner-cli, components/provider) \
             failed.\n\
             - missing target? `rustup target add wasm32-wasip2`\n\
             - host-only CLI (no compose-runner/run defaults): build with \
             `--no-default-features`\n"
        );
        std::process::exit(1);
    }

    let built = target_dir.join("wasm32-wasip2").join("embed");
    for artifact in ["runner_cli.wasm", "provider.wasm"] {
        std::fs::copy(built.join(artifact), out_dir.join(artifact))
            .unwrap_or_else(|e| panic!("copying {artifact} out of the inner build: {e}"));
    }
}

/// The inner build keeps only what it needs: registry/network knobs
/// and the ambient environment. Everything cargo set for *this* build
/// script — and the host-targeted compiler overrides — is scrubbed.
/// Deliberately including `CARGO_TARGET_WASM32_WASIP2_RUSTFLAGS` (the
/// `CARGO` prefix): the embedded components build with the workspace's
/// own flags everywhere, ambient wasm-target overrides included.
fn keep_env(k: &str) -> bool {
    if k == "CARGO_HOME"
        || k.starts_with("CARGO_NET_")
        || k.starts_with("CARGO_HTTP_")
        || k.starts_with("CARGO_REGISTR")
    {
        return true;
    }
    if k.starts_with("CARGO") {
        return false;
    }
    !matches!(
        k,
        "RUSTFLAGS" | "RUSTC" | "RUSTDOC" | "RUSTC_WORKSPACE_WRAPPER" | "OUT_DIR"
    )
}
