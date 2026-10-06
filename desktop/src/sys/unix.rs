//! Small helpers around external desktop tools (all optional).

use std::io::{Read, Write};
use std::net::UdpSocket;
use std::process::{Command, Stdio};

pub fn have(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

pub fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").map(|s| s.to_uppercase().contains("GNOME")).unwrap_or(false)
}

fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Fire-and-forget desktop notification.
pub fn notify(title: &str, body: &str) {
    if !have("notify-send") {
        eprintln!("[notify] {}: {}", title, body);
        return;
    }
    let child = Command::new("notify-send")
        .args(["-a", "Ferry", "-i", "phone", title, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut c) = child {
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
}

/// Fallback clipboard writer used when the GNOME extension is not connected.
pub fn clipboard_set(text: &str) -> bool {
    let cmds: Vec<(&str, Vec<&str>)> = if wayland() {
        vec![("wl-copy", vec![]), ("xclip", vec!["-selection", "clipboard"]), ("xsel", vec!["-ib"])]
    } else {
        vec![("xclip", vec!["-selection", "clipboard"]), ("xsel", vec!["-ib"]), ("wl-copy", vec![])]
    };
    for (c, args) in cmds {
        if !have(c) {
            continue;
        }
        if let Ok(mut ch) = Command::new(c)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut i) = ch.stdin.take() {
                let _ = i.write_all(text.as_bytes());
            }
            // xclip/wl-copy fork into the background to serve the selection.
            std::thread::spawn(move || {
                let _ = ch.wait();
            });
            return true;
        }
    }
    false
}

/// Fallback clipboard reader (works on X11 and wlroots/KDE Wayland, not GNOME Wayland).
pub fn clipboard_get() -> Option<String> {
    let cmds: Vec<(&str, Vec<&str>)> = vec![
        ("wl-paste", vec!["-n", "-t", "text"]),
        ("xclip", vec!["-selection", "clipboard", "-o"]),
        ("xsel", vec!["-ob"]),
    ];
    for (c, args) in cmds {
        if c == "wl-paste" && !wayland() {
            continue;
        }
        if !have(c) {
            continue;
        }
        if let Ok(out) = Command::new(c).args(&args).stderr(Stdio::null()).output() {
            if out.status.success() {
                if let Ok(s) = String::from_utf8(out.stdout) {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// Whether the current Wayland selection is flagged as a password (KeePassXC etc.).
pub fn clipboard_is_secret() -> bool {
    if let Ok(out) = Command::new("wl-paste").arg("-l").stderr(Stdio::null()).output() {
        return String::from_utf8_lossy(&out.stdout).contains("x-kde-passwordManagerHint");
    }
    false
}

/// Event-driven clipboard watching for non-GNOME Wayland compositors
/// (wlroots, KDE). Calls `on_change` for every new selection. Blocks.
pub fn watch_clipboard_wl(on_change: impl Fn()) {
    if !wayland() || !have("wl-paste") {
        return;
    }
    let child = Command::new("wl-paste")
        .args(["-t", "text", "--watch", "echo"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return };
    let mut out = child.stdout.take().unwrap();
    let mut buf = [0u8; 256];
    loop {
        match out.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                for _ in buf[..n].iter().filter(|b| **b == b'\n') {
                    on_change();
                }
            }
        }
    }
    let _ = child.wait();
    eprintln!("ferry: wl-paste clipboard watcher ended (compositor may not support it)");
}

/// IPv4 addresses of this machine that the phone could reach.
pub fn local_addrs() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(out) = Command::new("ip").args(["-o", "-4", "addr", "show", "scope", "global"]).output() {
        for l in String::from_utf8_lossy(&out.stdout).lines() {
            let t: Vec<&str> = l.split_whitespace().collect();
            if let Some(i) = t.iter().position(|x| *x == "inet") {
                if let Some(a) = t.get(i + 1) {
                    let ip = a.split('/').next().unwrap_or("");
                    let ifname = t.get(1).unwrap_or(&"");
                    if !ip.is_empty() && !ifname.starts_with("docker") && !ifname.starts_with("br-") && !ifname.starts_with("virbr") {
                        v.push(ip.to_string());
                    }
                }
            }
        }
    }
    if v.is_empty() {
        if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
            if s.connect("192.0.2.1:9").is_ok() {
                if let Ok(a) = s.local_addr() {
                    v.push(a.ip().to_string());
                }
            }
        }
    }
    v
}

/// Notification for received files (Linux: same as a normal notification).
pub fn notify_file(title: &str, body: &str, _open: &std::path::Path) {
    notify(title, body);
}

/// Daemon events (pairing etc.). The GNOME extension gets them over IPC; nothing to do here.
pub fn on_event(_fields: &[&str]) {}

/// The daemon's main thread just waits; all work happens on the listener threads.
pub fn main_loop(_st: std::sync::Arc<crate::daemon::State>) -> std::io::Result<()> {
    loop {
        std::thread::park();
    }
}

/// File chooser for `ferry send --pick`.
pub fn pick_files() -> Vec<std::path::PathBuf> {
    let out = if have("zenity") {
        Command::new("zenity")
            .args(["--file-selection", "--multiple", "--separator=\n", "--title=Send to phone"])
            .output()
    } else if have("kdialog") {
        Command::new("kdialog").args(["--getopenfilename", ".", "--multiple", "--separate-output"]).output()
    } else {
        eprintln!("ferry: install zenity (or kdialog) for the file picker");
        return Vec::new();
    };
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(std::path::PathBuf::from)
            .collect(),
        _ => Vec::new(),
    }
}

pub fn error_box(msg: &str) {
    eprintln!("{}", msg);
}

pub fn init_logging() {}

/// Notification for a transfer waiting in Incoming, with Accept / Decline buttons when
/// the notification server supports actions (libnotify >= 0.7.10, e.g. Ubuntu 24.04).
/// Never a dialog: it just sits in the notification list.
pub fn notify_incoming(title: &str, body: &str, on_choice: Box<dyn FnOnce(&str) + Send>) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        if !have("notify-send") {
            eprintln!("[incoming] {}: {}", title, body);
            return;
        }
        let supports_actions = Command::new("notify-send")
            .arg("--help")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("--action"))
            .unwrap_or(false);
        if !supports_actions {
            notify(&title, &format!("{} Use the Ferry menu (or `ferry incoming`) to accept.", body));
            return;
        }
        let out = Command::new("notify-send")
            .args(["-a", "Ferry", "-i", "phone", "-u", "normal", "--wait"])
            .args(["-A", "accept=Accept", "-A", "decline=Decline"])
            .arg(&title)
            .arg(&body)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        if let Ok(o) = out {
            let choice = String::from_utf8_lossy(&o.stdout).trim().to_string();
            on_choice(&choice);
        }
    });
}
