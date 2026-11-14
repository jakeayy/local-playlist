#!/usr/bin/env bash
# Build the browser UI into web/dist (embedded into the server by build.rs).
#
# Requires: a nightly toolchain with the wasm32 target, wasm-bindgen-cli
# matching the wasm-bindgen version in Cargo.lock, and (optional) wasm-opt
# from Binaryen to shrink the output:
#   rustup toolchain install nightly --profile minimal --target wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version <version from Cargo.lock> --locked
set -euo pipefail
cd "$(dirname "$0")/.."

# Nightly is needed because GPUI's web backend enables wasm threading support.
cargo +nightly rustc --lib --crate-type cdylib --target wasm32-unknown-unknown \
    --profile wasm-release --no-default-features --features ui
rm -rf web/dist
wasm-bindgen --target web --no-typescript --out-dir web/dist \
    target/wasm32-unknown-unknown/wasm-release/local_playlist.wasm
if command -v wasm-opt >/dev/null; then
    # Features match what rustc enables by default for wasm32-unknown-unknown,
    # plus threads: GPUI's scheduler contains (unused) atomics.
    wasm-opt -Oz --strip-debug --strip-producers \
        --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
        --enable-mutable-globals --enable-reference-types --enable-multivalue --enable-threads \
        -o web/dist/local_playlist_bg.wasm web/dist/local_playlist_bg.wasm
else
    echo "note: wasm-opt not found; skipping size optimization" >&2
fi
gzip -9 -k -f web/dist/local_playlist_bg.wasm web/dist/local_playlist.js
ls -lh web/dist
echo "Web UI built. Now build the server, e.g.: cargo build --release"
