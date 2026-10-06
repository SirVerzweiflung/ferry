//! `ferryd`: the Windows background app (tray icon, no console window).
//! Also used by Explorer's "Send to" menu: `ferryd send FILE...`.
//! On Linux it behaves like `ferry daemon` (use the `ferry` binary there).
#![cfg_attr(windows, windows_subsystem = "windows")]

use ferry::{daemon, ipc, sys};
use std::path::PathBuf;
use std::time::Duration;

fn connect_or_start() -> Result<ipc::Client, String> {
    if let Ok(c) = ipc::Client::connect() {
        return Ok(c);
    }
    // Not running yet: start ourselves in the background and wait for it.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe).spawn().map_err(|e| e.to_string())?;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(c) = ipc::Client::connect() {
            return Ok(c);
        }
    }
    Err("Ferry did not start".into())
}

fn absolute<S: AsRef<str>>(files: &[S]) -> Vec<String> {
    files
        .iter()
        .map(|f| PathBuf::from(f.as_ref()))
        .map(|f| std::fs::canonicalize(&f).unwrap_or(f).to_string_lossy().to_string())
        .map(|s| s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s))
        .collect()
}

fn forward(fields: &[&str]) -> Result<String, String> {
    let mut c = connect_or_start()?;
    c.send(fields).map_err(|e| e.to_string())?;
    c.result()
}

fn main() {
    sys::init_logging();
    std::panic::set_hook(Box::new(|info| {
        eprintln!("ferry: crashed: {}", info);
        sys::error_box(&format!("Ferry crashed:\n\n{}", info));
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let res = match a.as_slice() {
        [] | ["daemon"] => {
            eprintln!("ferry {}: starting", env!("CARGO_PKG_VERSION"));
            daemon::run().map(|_| String::new()).map_err(|e| e.to_string())
        }
        ["send", "--choose", files @ ..] if !files.is_empty() => (|| {
            let mut c = connect_or_start()?;
            c.send(&["devices"]).map_err(|e| e.to_string())?;
            let mut mine = Vec::new();
            let mut near = Vec::new();
            while let Ok(Some(f)) = c.recv() {
                if f[0] == "devend" {
                    break;
                }
                if f[0] == "dev" {
                    let entry = (f[1].clone(), f[2].clone());
                    if f[4] == "paired" { mine.push(entry) } else { near.push(entry) }
                }
            }
            let mut items = vec![(String::new(), "-- My devices".to_string())];
            items.extend(mine);
            items.push((String::new(), "-- Nearby (they have to accept)".into()));
            items.extend(near);
            let Some(id) = sys::choose_device(&format!("Send {} file(s) to", files.len()), &items) else {
                return Ok(String::new());
            };
            let abs = absolute(files);
            let mut fields: Vec<&str> = vec!["sendfiles", &id];
            fields.extend(abs.iter().map(|s| s.as_str()));
            c.send(&fields).map_err(|e| e.to_string())?;
            c.result()
        })(),
        ["send", files @ ..] => {
            let files: Vec<PathBuf> = if files.is_empty() { sys::pick_files() } else { files.iter().map(PathBuf::from).collect() };
            if files.is_empty() {
                return;
            }
            let abs = absolute(&files.iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>());
            let mut fields: Vec<&str> = vec!["sendfiles", ""];
            fields.extend(abs.iter().map(|s| s.as_str()));
            forward(&fields)
        }
        ["clip"] => forward(&["sendclip"]),
        _ => Err("usage: ferryd [daemon | send [--choose] FILE... | clip]".into()),
    };
    if let Err(e) = res {
        eprintln!("ferry: {}", e);
        if e.contains("already running") {
            return; // e.g. started twice at login
        }
        sys::error_box(&format!("Ferry: {}", e));
    }
}
