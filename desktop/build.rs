// - Sets `cfg(win)` for Windows builds. FERRY_WINCHECK=1 type-checks the Windows code on Linux
//   (`FERRY_WINCHECK=1 cargo check`).
// - For Windows GNU builds (e.g. cross-compiling from Linux with mingw-w64), embeds the app
//   icon, version info and an application manifest into the .exe files (needs `windres`).
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
    let manifest = out.join("ferry.manifest");
    let obj = out.join("ferry-res.o");

    // Version info (Explorer -> Properties -> Details) and an application manifest.
    // Binaries without either are judged more suspicious by heuristic virus scanners.
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut nums: Vec<u32> = ver.split(['.', '-']).filter_map(|x| x.parse().ok()).take(3).collect();
    nums.resize(3, 0);
    let v4 = format!("{},{},{},0", nums[0], nums[1], nums[2]);
    let manifest_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Ferry" version="1.0.0.0"/>
  <description>Ferry - share files and the clipboard with your phone</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security><requestedPrivileges><requestedExecutionLevel level="asInvoker" uiAccess="false"/></requestedPrivileges></security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application><supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/></application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#;
    if std::fs::write(&manifest, manifest_xml).is_err() {
        return;
    }
    let path = |p: &PathBuf| p.to_string_lossy().replace('\\', "/");
    let rc_text = format!(
        r#"1 ICON "{ico}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {v4}
PRODUCTVERSION {v4}
FILEFLAGSMASK 0x3f
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Ferry (open source)"
      VALUE "FileDescription", "Ferry - share files and the clipboard with your phone"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "Ferry"
      VALUE "LegalCopyright", "MIT License"
      VALUE "ProductName", "Ferry"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = path(&ico),
        manifest = path(&manifest),
        v4 = v4,
        ver = ver
    );
    if std::fs::write(&rc, rc_text).is_err() {
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
    println!("cargo:warning=windres not found - building without icon/version info (install mingw-w64)");
}
