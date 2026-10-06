//! "Incoming": transfers from unpaired (nearby) devices wait here until the user
//! accepts them (moved to Downloads) or declines them. Unanswered transfers are
//! deleted after `incoming_hours`. Stored in <config>/incoming/<id>/ with a `meta`
//! file written last, so half-received transfers are recognised and removed.

use crate::crypto::{random_array, to_hex};
use crate::ipc::{escape, unescape};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Limits that keep unpaired devices from filling the disk or spamming.
pub const MAX_PENDING: usize = 10;
pub const MAX_PENDING_PER_SENDER: usize = 2;

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Clone, Debug)]
pub struct Pending {
    pub id: String,
    pub from_name: String,
    pub from_id: [u8; 16],
    pub ip: String,
    pub time: u64,
    pub files: Vec<(String, u64)>,
    pub text: Option<String>,
    pub dir: PathBuf,
}

impl Pending {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.1).sum()
    }

    /// "3 files (12.4 MB)" / "a text" / "photo.jpg (2.1 MB) and a text"
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        match self.files.len() {
            0 => {}
            1 => parts.push(format!("{} ({})", self.files[0].0, human(self.bytes()))),
            n => parts.push(format!("{} files ({})", n, human(self.bytes()))),
        }
        if self.text.is_some() {
            parts.push("a text".into());
        }
        parts.join(" and ")
    }
}

pub fn human(b: u64) -> String {
    let f = b as f64;
    if f >= 1e9 {
        format!("{:.1} GB", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1} MB", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.0} KB", f / 1e3)
    } else {
        format!("{} B", b)
    }
}

pub struct Inbox {
    root: PathBuf,
    items: Mutex<Vec<Pending>>,
    /// Wakes the expiry thread when something new arrives.
    pub changed: Condvar,
}

fn write_meta(p: &Pending) -> io::Result<()> {
    let mut s = format!(
        "from\t{}\nfromid\t{}\nip\t{}\ntime\t{}\n",
        escape(&p.from_name),
        to_hex(&p.from_id),
        p.ip,
        p.time
    );
    for (n, sz) in &p.files {
        s.push_str(&format!("file\t{}\t{}\n", escape(n), sz));
    }
    if let Some(t) = &p.text {
        s.push_str(&format!("text\t{}\n", escape(t)));
    }
    let tmp = p.dir.join("meta.tmp");
    fs::write(&tmp, s)?;
    fs::rename(tmp, p.dir.join("meta"))
}

fn read_meta(dir: &Path) -> Option<Pending> {
    let s = fs::read_to_string(dir.join("meta")).ok()?;
    let mut p = Pending {
        id: dir.file_name()?.to_string_lossy().to_string(),
        from_name: String::new(),
        from_id: [0; 16],
        ip: String::new(),
        time: 0,
        files: Vec::new(),
        text: None,
        dir: dir.to_path_buf(),
    };
    for line in s.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["from", v] => p.from_name = unescape(v),
            ["fromid", v] => {
                if let Some(b) = crate::crypto::from_hex(v) {
                    if b.len() == 16 {
                        p.from_id.copy_from_slice(&b);
                    }
                }
            }
            ["ip", v] => p.ip = v.to_string(),
            ["time", v] => p.time = v.parse().unwrap_or(0),
            ["file", n, sz] => p.files.push((unescape(n), sz.parse().unwrap_or(0))),
            ["text", v] => p.text = Some(unescape(v)),
            _ => {}
        }
    }
    Some(p)
}

impl Inbox {
    pub fn load(root: PathBuf) -> Inbox {
        let _ = fs::create_dir_all(&root);
        let mut items = Vec::new();
        if let Ok(rd) = fs::read_dir(&root) {
            for e in rd.flatten() {
                let d = e.path();
                match read_meta(&d) {
                    Some(p) if d.is_dir() => items.push(p),
                    _ => {
                        let _ = fs::remove_dir_all(&d); // incomplete or foreign
                    }
                }
            }
        }
        items.sort_by_key(|p| p.time);
        Inbox { root, items: Mutex::new(items), changed: Condvar::new() }
    }

    pub fn list(&self) -> Vec<Pending> {
        self.items.lock().unwrap().clone()
    }

    pub fn pending_bytes(&self) -> u64 {
        self.items.lock().unwrap().iter().map(|p| p.bytes()).sum()
    }

    /// Whether another transfer from `from` may start.
    pub fn has_room_for(&self, from: &[u8; 16], ip: &str) -> bool {
        let g = self.items.lock().unwrap();
        g.len() < MAX_PENDING
            && g.iter().filter(|p| &p.from_id == from || p.ip == ip).count() < MAX_PENDING_PER_SENDER
    }

    /// A fresh directory for a transfer that is being received.
    pub fn begin(&self, from_name: &str, from_id: [u8; 16], ip: &str) -> io::Result<Pending> {
        let id = to_hex(&random_array::<5>());
        let dir = self.root.join(&id);
        fs::create_dir_all(&dir)?;
        Ok(Pending {
            id,
            from_name: from_name.to_string(),
            from_id,
            ip: ip.to_string(),
            time: now_secs(),
            files: Vec::new(),
            text: None,
            dir,
        })
    }

    pub fn commit(&self, p: Pending) -> io::Result<()> {
        write_meta(&p)?;
        self.items.lock().unwrap().push(p);
        self.changed.notify_all();
        Ok(())
    }

    pub fn discard(p: &Pending) {
        let _ = fs::remove_dir_all(&p.dir);
    }

    /// Removes and returns an item (by id, or a unique id prefix).
    pub fn take(&self, id: &str) -> Option<Pending> {
        let mut g = self.items.lock().unwrap();
        let matches: Vec<usize> = (0..g.len()).filter(|i| g[*i].id.starts_with(id)).collect();
        if matches.len() != 1 {
            return None;
        }
        Some(g.remove(matches[0]))
    }

    pub fn take_all(&self) -> Vec<Pending> {
        std::mem::take(&mut *self.items.lock().unwrap())
    }

    pub fn ids_from(&self, from: &[u8; 16]) -> Vec<String> {
        self.items.lock().unwrap().iter().filter(|p| &p.from_id == from).map(|p| p.id.clone()).collect()
    }

    /// Blocks forever, deleting transfers older than `hours()`. Sleeps until the next
    /// expiry (or until something new arrives) - never polls.
    pub fn expiry_loop(&self, hours: impl Fn() -> u64, on_change: impl Fn()) {
        let mut g = self.items.lock().unwrap();
        loop {
            let ttl = hours().max(1) * 3600;
            let now = now_secs();
            let before = g.len();
            g.retain(|p| {
                let keep = p.time + ttl > now;
                if !keep {
                    Inbox::discard(p);
                }
                keep
            });
            if g.len() != before {
                drop(g);
                on_change();
                g = self.items.lock().unwrap();
                continue;
            }
            let wait = g.iter().map(|p| p.time + ttl).min().map(|t| t.saturating_sub(now) + 1);
            g = match wait {
                Some(secs) => self.changed.wait_timeout(g, Duration::from_secs(secs)).unwrap().0,
                None => self.changed.wait(g).unwrap(),
            };
        }
    }
}
