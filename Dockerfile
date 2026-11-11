# Local Playlist server: web UI + API in one small binary.
#
#   docker build -t local-playlist .
#   docker run -d -p 8080:8080 -v lp-data:/data \
#       -e LOCAL_PLAYLIST_PASSWORD='choose-a-long-password' local-playlist
#
# yt-dlp (official master build) and QuickJS are downloaded into /data on
# first start, and yt-dlp is updated daily.

# ---- build: web UI (wasm, nightly) + server-only binary (stable)
FROM rust:1-bookworm AS build
ARG BINARYEN_VERSION=133
RUN rustup toolchain install nightly --profile minimal --target wasm32-unknown-unknown \
    && curl -fsSL "https://github.com/WebAssembly/binaryen/releases/download/version_${BINARYEN_VERSION}/binaryen-version_${BINARYEN_VERSION}-x86_64-linux.tar.gz" \
       | tar xz -C /opt && ln -s /opt/binaryen-version_${BINARYEN_VERSION}/bin/wasm-opt /usr/local/bin/wasm-opt
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
RUN WB=$(grep -A1 'name = "wasm-bindgen"$' Cargo.lock | sed -n 's/version = "\(.*\)"/\1/p') \
    && cargo install wasm-bindgen-cli --version "$WB" --locked
COPY . .
RUN ./scripts/build-web.sh \
    && cargo build --release --no-default-features --features server

# ---- runtime: only libc is needed (yt-dlp's build bundles its own Python)
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /data --uid 10001 app \
    && mkdir -p /data && chown app /data
COPY --from=build /src/target/release/local-playlist /usr/local/bin/local-playlist
USER app
ENV LOCAL_PLAYLIST_DATA=/data LOCAL_PLAYLIST_LISTEN=0.0.0.0:8080
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["tini", "--", "local-playlist", "--server"]
