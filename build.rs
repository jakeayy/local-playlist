// - Embed the web UI into the server when it has been built
//   (scripts/build-web.sh writes web/dist/).
// - Generate the table of icons the server hands to the web UI (web/icons/).
use std::fmt::Write as _;

fn main() {
    // Version string for `--version` (and bug reports): package version + git commit.
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|c| !c.is_empty());
    let version = match commit {
        Some(c) => format!("{} ({c})", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_string(),
    };
    println!("cargo::rustc-env=LOCAL_PLAYLIST_VERSION={version}");
    println!("cargo::rerun-if-changed=.git/HEAD");
    println!("cargo::rerun-if-changed=.git/refs/heads");
    println!("cargo::rustc-check-cfg=cfg(web_ui)");
    println!("cargo::rerun-if-changed=web/dist");
    println!("cargo::rerun-if-changed=web/icons");
    let dist = std::path::Path::new("web/dist");
    if dist.join("local_playlist_bg.wasm.gz").exists() && dist.join("local_playlist.js.gz").exists() {
        println!("cargo::rustc-cfg=web_ui");
    }

    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let mut names: Vec<String> = std::fs::read_dir("web/icons")
        .map(|it| it.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.retain(|n| n.ends_with(".svg"));
    names.sort();
    let mut out = String::from("static ICONS: &[(&str, &[u8])] = &[\n");
    for n in names {
        let _ = writeln!(out, "    ({n:?}, include_bytes!({:?})),", format!("{manifest}/web/icons/{n}"));
    }
    out.push_str("];\n");
    std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("icons.rs"), out).unwrap();
}
