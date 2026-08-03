#!/usr/bin/env bash
# Install the pinned non-Rust toolchain (wasmtime, wac, wasm-tools, just)
# into ~/.local/bin if not already present at the right version.
# Rust itself is pinned by rust-toolchain.toml; Node by the workflow.
set -euo pipefail

WASMTIME_VERSION="47.0.1"
WAC_VERSION="0.10.1"
WASM_TOOLS_VERSION="1.247.0"
JUST_VERSION="1.54.0"

BIN="${HOME}/.local/bin"
mkdir -p "$BIN"
export PATH="$BIN:$PATH"

arch="x86_64"

have() {
    command -v "$1" >/dev/null 2>&1 && "$1" --version 2>/dev/null | grep -qF "$2"
}

if ! have wasmtime "$WASMTIME_VERSION"; then
    echo "installing wasmtime $WASMTIME_VERSION"
    curl -sSfL --retry 3 "https://github.com/bytecodealliance/wasmtime/releases/download/v${WASMTIME_VERSION}/wasmtime-v${WASMTIME_VERSION}-${arch}-linux.tar.xz" |
        tar -xJ --strip-components=1 -C "$BIN" --wildcards '*/wasmtime'
fi

if ! have wac "$WAC_VERSION"; then
    echo "installing wac $WAC_VERSION"
    curl -sSfL --retry 3 -o "$BIN/wac" \
        "https://github.com/bytecodealliance/wac/releases/download/v${WAC_VERSION}/wac-cli-${arch}-unknown-linux-musl"
    chmod +x "$BIN/wac"
fi

if ! have wasm-tools "$WASM_TOOLS_VERSION"; then
    echo "installing wasm-tools $WASM_TOOLS_VERSION"
    curl -sSfL --retry 3 "https://github.com/bytecodealliance/wasm-tools/releases/download/v${WASM_TOOLS_VERSION}/wasm-tools-${WASM_TOOLS_VERSION}-${arch}-linux.tar.gz" |
        tar -xz --strip-components=1 -C "$BIN" --wildcards '*/wasm-tools'
fi

if ! have just "$JUST_VERSION"; then
    echo "installing just $JUST_VERSION"
    curl -sSfL --retry 3 "https://github.com/casey/just/releases/download/${JUST_VERSION}/just-${JUST_VERSION}-${arch}-unknown-linux-musl.tar.gz" |
        tar -xz -C "$BIN" just
fi

echo "toolchain ready:"
for t in wasmtime wac wasm-tools just; do
    printf '  %s: %s\n' "$t" "$($t --version 2>/dev/null | head -1)"
done
