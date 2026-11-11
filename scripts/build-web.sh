#!/usr/bin/env bash
# Build the browser UI into web/dist (embedded into the binary by build.rs).
#
# Requires: a nightly toolchain with the wasm32 target, and wasm-bindgen-cli
# matching the wasm-bindgen version in Cargo.lock:
#   rustup toolchain install nightly --profile minimal --target wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version <version from Cargo.lock> --locked
set -euo pipefail
cd "$(dirname "$0")/.."

# Nightly is needed because GPUI's web backend enables wasm threading support.
cargo +nightly rustc --lib --crate-type cdylib --target wasm32-unknown-unknown --release
rm -rf web/dist
wasm-bindgen --target web --no-typescript --out-dir web/dist \
    target/wasm32-unknown-unknown/release/local_playlist.wasm
if command -v wasm-opt >/dev/null; then
    wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
        --enable-mutable-globals --enable-reference-types \
        -o web/dist/local_playlist_bg.wasm web/dist/local_playlist_bg.wasm
fi
gzip -9 -k -f web/dist/local_playlist_bg.wasm web/dist/local_playlist.js
ls -lh web/dist
echo "Web UI built. Now: cargo build --release (it embeds web/dist)."
