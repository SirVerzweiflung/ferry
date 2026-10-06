//! Persistent state: config, device identity and paired peers.

use crate::crypto::{from_hex, random_array, to_hex};
use crate::proto::DEFAULT_PORT;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

pub fn config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("FERRY_HOME") {
        return PathBuf::from(d);
    }
    if cfg!(win) {
        if let Some(a) = std::env::var_os("APPDATA") {
            return PathBuf::from(a).join("Ferry");
        }
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".config"));
    base.join("ferry")
}

#[cfg(not(win))]
pub fn socket_path() -> PathBuf {
    if let Ok(s) = std::env::var("FERRY_SOCKET") {
        return PathBuf::from(s);
    }
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("ferry.sock");
    }
    let uid = fs::metadata(home()).map(|m| std::os::unix::fs::MetadataExt::uid(&m)).unwrap_or(0);
    PathBuf::from(format!("/tmp/ferry-{}.sock", uid))
}

fn hostname() -> String {
    if let Ok(n) = std::env::var("COMPUTERNAME") {
        if !n.is_empty() {
            return n;
        }
    }
    fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| fs::read_to_string("/etc/hostname"))
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Linux desktop".into())
}

fn default_download_dir() -> PathBuf {
    let h = home();
    if let Ok(s) = fs::read_to_string(h.join(".config/user-dirs.dirs")) {
        for line in s.lines() {
            if let Some(v) = line.strip_prefix("XDG_DOWNLOAD_DIR=") {
                let v = v.trim().trim_matches('"').replace("$HOME", &h.to_string_lossy());
                if !v.is_empty() {
                    return PathBuf::from(v);
                }
            }
        }
    }
    h.join("Downloads")
}

/// Atomic write with 0600 permissions.
pub fn write_private(path: &Path, data: &str) -> io::Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
        }
    }
    let tmp = path.with_extension("tmp");
    {
        let mut o = fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        // Windows: %APPDATA% is already private to the user.
        let mut f = o.open(&tmp)?;
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

#[derive(Clone, Debug)]
pub struct Config {
    pub name: String,
    pub port: u16,
    pub download_dir: PathBuf,
    pub auto_clipboard: bool,
    pub notifications: bool,
}

impl Config {
    pub fn load() -> Config {
        let mut c = Config {
            name: hostname(),
            port: DEFAULT_PORT,
            download_dir: default_download_dir(),
            auto_clipboard: true,
            notifications: true,
        };
        let path = config_dir().join("config");
        match fs::read_to_string(&path) {
            Ok(s) => {
                for line in s.lines() {
                    let line = line.trim();
                    if line.starts_with('#') {
                        continue;
                    }
                    if let Some((k, v)) = line.split_once('=') {
                        let _ = c.set(k.trim(), v.trim());
                    }
                }
            }
            Err(_) => {
                let _ = c.save();
            }
        }
        c
    }

    pub fn set(&mut self, key: &str, val: &str) -> Result<(), String> {
        let b = |v: &str| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
        match key {
            "name" if !val.is_empty() => self.name = val.to_string(),
            "port" => self.port = val.parse().map_err(|_| "invalid port".to_string())?,
            "download_dir" if !val.is_empty() => {
                self.download_dir = PathBuf::from(val.replacen('~', &home().to_string_lossy(), 1))
            }
            "auto_clipboard" => self.auto_clipboard = b(val),
            "notifications" => self.notifications = b(val),
            _ => return Err(format!("unknown setting '{}' (name, port, download_dir, auto_clipboard, notifications)", key)),
        }
        Ok(())
    }

    pub fn save(&self) -> io::Result<()> {
        let s = format!(
            "# Ferry settings. Restart not needed when changed via `ferry set`.\n\
             name = {}\nport = {}\ndownload_dir = {}\nauto_clipboard = {}\nnotifications = {}\n",
            self.name,
            self.port,
            self.download_dir.display(),
            self.auto_clipboard,
            self.notifications
        );
        write_private(&config_dir().join("config"), &s)
    }
}

pub fn load_identity() -> [u8; 16] {
    let path = config_dir().join("identity");
    if let Ok(s) = fs::read_to_string(&path) {
        if let Some(v) = from_hex(s.trim()) {
            if v.len() == 16 {
                return v.try_into().unwrap();
            }
        }
    }
    let id: [u8; 16] = random_array();
    let _ = write_private(&path, &to_hex(&id));
    id
}

#[derive(Clone, Debug)]
pub struct Peer {
    pub id: [u8; 16],
    pub name: String,
    pub key: [u8; 32],
    /// Last known "host:port".
    pub addr: Option<String>,
}

pub fn load_peers() -> Vec<Peer> {
    let mut out = Vec::new();
    let s = fs::read_to_string(config_dir().join("peers")).unwrap_or_default();
    for line in s.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 4 {
            continue;
        }
        let (Some(id), Some(key)) = (from_hex(f[0]), from_hex(f[2])) else { continue };
        if id.len() != 16 || key.len() != 32 {
            continue;
        }
        out.push(Peer {
            id: id.try_into().unwrap(),
            name: f[1].to_string(),
            key: key.try_into().unwrap(),
            addr: if f[3].is_empty() { None } else { Some(f[3].to_string()) },
        });
    }
    out
}

pub fn save_peers(peers: &[Peer]) -> io::Result<()> {
    let mut s = String::new();
    for p in peers {
        let name: String = p.name.chars().filter(|c| *c != '\t' && *c != '\n').collect();
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            to_hex(&p.id),
            name,
            to_hex(&p.key),
            p.addr.clone().unwrap_or_default()
        ));
    }
    write_private(&config_dir().join("peers"), &s)
}
