// Embed the web UI into the native binary when it has been built
// (scripts/build-web.sh writes web/dist/).
fn main() {
    println!("cargo::rustc-check-cfg=cfg(web_ui)");
    println!("cargo::rerun-if-changed=web/dist");
    let dist = std::path::Path::new("web/dist");
    if dist.join("local_playlist_bg.wasm.gz").exists() && dist.join("local_playlist.js.gz").exists() {
        println!("cargo::rustc-cfg=web_ui");
    }
}
