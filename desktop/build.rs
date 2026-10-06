// - Sets `cfg(win)` for Windows builds. FERRY_WINCHECK=1 type-checks the Windows code on Linux
//   (`FERRY_WINCHECK=1 cargo check`).
// - For Windows GNU builds (e.g. cross-compiling from Linux with mingw-w64), embeds the app
//   icon into the .exe files if `windres` is available. Otherwise this is silently skipped.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(win)");
    println!("cargo:rerun-if-env-changed=FERRY_WINCHECK");
    println!("cargo:rerun-if-changed=packaging/windows/ferry.ico");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" || std::env::var_os("FERRY_WINCHECK").is_some() {
        println!("cargo:rustc-cfg=win");
    }
    if os == "windows" && env == "gnu" {
        embed_icon();
    }
}

fn embed_icon() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = dir.join("packaging/windows/ferry.ico");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = out.join("ferry.rc");
    let obj = out.join("ferry-res.o");
    if std::fs::write(&rc, format!("1 ICON \"{}\"\n", ico.to_string_lossy().replace('\\', "/"))).is_err() {
        return;
    }
    let mut tools: Vec<String> = std::env::var("WINDRES").ok().into_iter().collect();
    tools.push("x86_64-w64-mingw32-windres".into());
    tools.push("windres".into());
    for t in tools {
        let ok = Command::new(&t)
            .arg(&rc)
            .args(["-O", "coff", "-o"])
            .arg(&obj)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            println!("cargo:rustc-link-arg-bins={}", obj.display());
            return;
        }
    }
}
