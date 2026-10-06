//! The background daemon.
//!
//! * My devices (paired with a code): files are saved directly, the clipboard syncs
//!   automatically between all of them (and is passed on, so every paired device gets it).
//! * Nearby devices (any other Ferry device on the network): can be sent to; what they
//!   send waits in Incoming until accepted, and is deleted after `incoming_hours`.
//! * Sends to a device that is not reachable are queued and retried when it shows up.
//!
//! Idle cost: threads blocked in accept()/recv()/condvar waits - no polling.

use crate::crypto::to_hex;
use crate::inbox::{self, Inbox, Pending};
use crate::ipc;
use crate::proto::*;
use crate::store::*;
use crate::sys;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PAIR_WINDOW: Duration = Duration::from_secs(300);
const PAIR_ATTEMPTS: u32 = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const IO_TIMEOUT: Duration = Duration::from_secs(60);
const SCAN_WAIT: Duration = Duration::from_millis(700);
const NEARBY_FRESH: Duration = Duration::from_secs(180);
const QUEUE_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
const RETRY_STEPS: [u64; 5] = [30, 120, 600, 1800, 3600];
const GUEST_RATE_WINDOW: Duration = Duration::from_secs(600);
const GUEST_RATE_MAX: usize = 20;

struct Pairing {
    code: String,
    expires: Instant,
    attempts: u32,
}

#[derive(Clone)]
pub struct Nearby {
    pub id: [u8; 16],
    pub name: String,
    pub kind: u8,
    pub addr: String,
    pub seen: Instant,
    pub accepts: bool,
}

/// One entry of the device list shown to the user.
#[derive(Clone)]
pub struct DevInfo {
    pub id: [u8; 16],
    pub name: String,
    pub kind: u8,
    pub paired: bool,
    pub online: bool,
    pub addr: String,
}

#[derive(Clone)]
pub enum Payload {
    Files(Vec<PathBuf>),
    Text(String),
}

impl Payload {
    fn describe(&self) -> String {
        match self {
            Payload::Files(p) if p.len() == 1 => p[0].file_name().unwrap_or_default().to_string_lossy().to_string(),
            Payload::Files(p) => format!("{} files", p.len()),
            Payload::Text(_) => "text".into(),
        }
    }
}

struct Job {
    id: u64,
    target: [u8; 16],
    target_name: String,
    payload: Payload,
    /// Files already delivered (paired targets save each file as it arrives).
    done: usize,
    created: Instant,
    next: Instant,
    attempts: u32,
}

enum SendErr {
    /// Not reachable / connection lost: queue and retry.
    Unreachable(String),
    /// The other side said no (or a local problem): don't retry.
    Refused(String),
}

impl From<io::Error> for SendErr {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::Other | io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => {
                SendErr::Refused(e.to_string())
            }
            _ => SendErr::Unreachable(e.to_string()),
        }
    }
}

/// UDP ports that presence/lookup broadcasts go to: the standard port, our own port and,
/// for tests with several instances on one machine, FERRY_DISCOVERY_PORTS="47810,47811".
fn broadcast_ports(own: u16) -> Vec<u16> {
    let mut v = vec![DEFAULT_PORT];
    if own != DEFAULT_PORT {
        v.push(own);
    }
    if let Ok(extra) = std::env::var("FERRY_DISCOVERY_PORTS") {
        v.extend(extra.split(',').filter_map(|p| p.trim().parse::<u16>().ok()).filter(|p| *p != own && *p != DEFAULT_PORT));
    }
    v
}

fn unreachable(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, msg)
}

type Sub = Arc<Mutex<ipc::Stream>>;

pub struct State {
    id: [u8; 16],
    cfg: Mutex<Config>,
    peers: Mutex<Vec<Peer>>,
    pairing: Mutex<Option<Pairing>>,
    /// (connection, handles-clipboard)
    subs: Mutex<Vec<(Sub, bool)>>,
    /// Latest clipboard text known on this desktop (avoids echo loops).
    last_clip: Mutex<String>,
    /// Clipboard text to send, and the device it came from (not sent back there).
    clip_queue: (Mutex<Option<(String, Option<[u8; 16]>)>>, Condvar),
    nearby: Mutex<HashMap<[u8; 16], Nearby>>,
    udp: Option<UdpSocket>,
    inbox: Inbox,
    outbox: (Mutex<Vec<Job>>, Condvar),
    next_job: Mutex<u64>,
    blocked: Mutex<Vec<Blocked>>,
    guest_rate: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl ServerCtx for State {
    fn peer_key(&self, id: &[u8; 16]) -> Option<[u8; 32]> {
        self.peers.lock().unwrap().iter().find(|p| &p.id == id).map(|p| p.key)
    }
    fn pair_code(&self) -> Option<String> {
        let mut g = self.pairing.lock().unwrap();
        match &*g {
            Some(p) if p.expires > Instant::now() => Some(p.code.clone()),
            Some(_) => {
                *g = None;
                None
            }
            None => None,
        }
    }
    fn pair_failed(&self) {
        let mut g = self.pairing.lock().unwrap();
        if let Some(p) = g.as_mut() {
            p.attempts += 1;
            if p.attempts >= PAIR_ATTEMPTS {
                *g = None;
                drop(g);
                self.broadcast(&["pairfailed", "too many wrong codes - start pairing again"]);
                sys::notify("Pairing cancelled", "Too many wrong codes were entered.");
            }
        }
    }
    fn guest_status(&self, id: &[u8; 16], ip: IpAddr) -> u8 {
        if !self.cfg.lock().unwrap().visible {
            return ST_NO_GUESTS;
        }
        let ips = ip.to_string();
        if self.blocked.lock().unwrap().iter().any(|b| &b.id == id || (!b.ip.is_empty() && b.ip == ips)) {
            return ST_NO_GUESTS;
        }
        {
            let mut r = self.guest_rate.lock().unwrap();
            let v = r.entry(ip).or_default();
            v.retain(|t| t.elapsed() < GUEST_RATE_WINDOW);
            if v.len() >= GUEST_RATE_MAX {
                return ST_BUSY;
            }
            v.push(Instant::now());
        }
        if !self.inbox.has_room_for(id, &ips) {
            return ST_BUSY;
        }
        ST_OK
    }
}

impl State {
    fn port(&self) -> u16 {
        self.cfg.lock().unwrap().port
    }
    fn name(&self) -> String {
        self.cfg.lock().unwrap().name.clone()
    }
    fn notify(&self, t: &str, b: &str) {
        if self.cfg.lock().unwrap().notifications {
            sys::notify(t, b);
        }
    }
    fn notify_file(&self, t: &str, b: &str, open: &Path) {
        if self.cfg.lock().unwrap().notifications {
            sys::notify_file(t, b, open);
        }
    }

    fn broadcast(&self, fields: &[&str]) {
        sys::on_event(fields);
        let l = ipc::line(fields);
        self.subs.lock().unwrap().retain(|(s, _)| s.lock().unwrap().write_all(l.as_bytes()).is_ok());
    }

    fn has_clip_handler(&self) -> bool {
        self.subs.lock().unwrap().iter().any(|(_, c)| *c)
    }

    fn save_peers(&self) {
        let peers = self.peers.lock().unwrap().clone();
        if let Err(e) = save_peers(&peers) {
            eprintln!("ferry: cannot save peers: {}", e);
        }
    }

    /// Insert or update a peer.
    fn upsert_peer(&self, p: Peer) {
        {
            let mut g = self.peers.lock().unwrap();
            g.retain(|x| x.id != p.id);
            g.insert(0, p);
        }
        self.save_peers();
        self.broadcast(&["devices"]);
    }

    fn touch_peer(&self, id: &[u8; 16], name: &str, addr: Option<String>, kind: u8) {
        let mut changed = false;
        {
            let mut g = self.peers.lock().unwrap();
            if let Some(p) = g.iter_mut().find(|p| &p.id == id) {
                if p.name != name && !name.is_empty() {
                    p.name = name.to_string();
                    changed = true;
                }
                if addr.is_some() && p.addr != addr {
                    p.addr = addr;
                    changed = true;
                }
                if kind != 0 && p.kind != kind {
                    p.kind = kind;
                    changed = true;
                }
            }
        }
        if changed {
            self.save_peers();
        }
    }

    fn paired(&self, id: &[u8; 16]) -> Option<Peer> {
        self.peers.lock().unwrap().iter().find(|p| &p.id == id).cloned()
    }

    /// The "main" device: the phone if one is paired, else the most recently paired device.
    fn main_peer(&self) -> Option<Peer> {
        let g = self.peers.lock().unwrap();
        g.iter().find(|p| p.kind == KIND_PHONE).or_else(|| g.first()).cloned()
    }

    /// Sets the clipboard of this computer (through the GNOME extension when available).
    fn set_desktop_clipboard(&self, text: &str) {
        *self.last_clip.lock().unwrap() = text.to_string();
        if self.has_clip_handler() {
            self.broadcast(&["setclip", text]);
        } else if !sys::clipboard_set(text) {
            eprintln!("ferry: no way to set the clipboard (install the GNOME extension or wl-clipboard/xclip)");
        }
    }

    fn queue_clip(&self, text: String, from: Option<[u8; 16]>) {
        if !self.cfg.lock().unwrap().auto_clipboard {
            return;
        }
        let (m, cv) = &self.clip_queue;
        *m.lock().unwrap() = Some((text, from));
        cv.notify_one();
    }

    /// Called whenever the clipboard of this computer changed.
    pub fn clipboard_changed(&self, text: String) {
        if text.is_empty() || text.len() > MAX_CLIP {
            return;
        }
        {
            let mut last = self.last_clip.lock().unwrap();
            if *last == text {
                return;
            }
            *last = text.clone();
        }
        self.queue_clip(text, None);
    }

    // ------------------------------------------------------------ nearby devices

    fn presence(&self, magic: &[u8; 4]) -> Vec<u8> {
        let c = self.cfg.lock().unwrap();
        let flags = if c.visible { PRES_GUESTS } else { 0 };
        presence_packet(magic, &self.id, c.port, KIND_PC, flags, &c.name)
    }

    fn send_presence(&self, magic: &[u8; 4]) {
        if let Some(u) = &self.udp {
            let p = self.presence(magic);
            for port in broadcast_ports(self.port()) {
                let _ = u.send_to(&p, ("255.255.255.255", port));
            }
        }
    }

    /// Asks the network who is there; answers arrive in udp_loop.
    fn scan(&self) {
        self.send_presence(PRES_QUERY);
        std::thread::sleep(SCAN_WAIT);
    }

    pub fn devices(&self, scan: bool) -> Vec<DevInfo> {
        if scan {
            self.scan();
        }
        let near = self.nearby.lock().unwrap().clone();
        let fresh = |id: &[u8; 16]| near.get(id).map(|n| n.seen.elapsed() < NEARBY_FRESH).unwrap_or(false);
        let mut out: Vec<DevInfo> = Vec::new();
        let main = self.main_peer().map(|p| p.id);
        let mut peers = self.peers.lock().unwrap().clone();
        peers.sort_by_key(|p| Some(p.id) != main); // main device first
        for p in peers {
            out.push(DevInfo {
                id: p.id,
                name: p.name.clone(),
                kind: p.kind,
                paired: true,
                online: fresh(&p.id),
                addr: p.addr.clone().unwrap_or_default(),
            });
        }
        let mut others: Vec<&Nearby> = near
            .values()
            .filter(|n| n.accepts && n.seen.elapsed() < NEARBY_FRESH && !out.iter().any(|d| d.id == n.id))
            .collect();
        others.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        for n in others {
            out.push(DevInfo {
                id: n.id,
                name: n.name.clone(),
                kind: n.kind,
                paired: false,
                online: true,
                addr: n.addr.clone(),
            });
        }
        out
    }

    /// Finds a device by id (hex prefix) or name; empty = main device.
    fn find_target(&self, sel: &str) -> Result<DevInfo, String> {
        let pick = |list: &[DevInfo]| -> Option<DevInfo> {
            if sel.is_empty() {
                return list.iter().find(|d| d.paired).cloned();
            }
            let s = sel.to_lowercase();
            list.iter()
                .find(|d| to_hex(&d.id) == s || d.name.to_lowercase() == s)
                .or_else(|| {
                    list.iter().find(|d| to_hex(&d.id).starts_with(&s) || d.name.to_lowercase().starts_with(&s))
                })
                .cloned()
        };
        if let Some(d) = pick(&self.devices(false)) {
            return Ok(d);
        }
        if sel.is_empty() {
            return Err("no paired devices yet - pair one first (ferry pair)".into());
        }
        pick(&self.devices(true)).ok_or_else(|| format!("no device called '{}' found", sel))
    }

    // ------------------------------------------------------------ API for CLI / tray / extension

    pub fn auto_clipboard(&self) -> bool {
        self.cfg.lock().unwrap().auto_clipboard
    }

    pub fn visible(&self) -> bool {
        self.cfg.lock().unwrap().visible
    }

    pub fn download_dir(&self) -> PathBuf {
        self.cfg.lock().unwrap().download_dir.clone()
    }

    pub fn device_name(&self) -> String {
        self.name()
    }

    pub fn peer_names(&self) -> Vec<String> {
        self.peers.lock().unwrap().iter().map(|p| p.name.clone()).collect()
    }

    pub fn set_option(&self, key: &str, val: &str) -> Result<String, String> {
        let mut c = self.cfg.lock().unwrap();
        c.set(key, val)?;
        c.save().map_err(|e| e.to_string())?;
        let (auto, vis) = (c.auto_clipboard, c.visible);
        drop(c);
        self.broadcast(&["config", "auto_clipboard", if auto { "1" } else { "0" }]);
        self.broadcast(&["config", "visible", if vis { "1" } else { "0" }]);
        if key == "visible" {
            // Tell everyone right away (when hiding: "no longer accepting", so lists update).
            self.send_presence(PRES_HERE);
        }
        Ok("saved (a new port takes effect after restart)".into())
    }

    /// Opens a 5-minute pairing window. Returns (formatted code, local addresses).
    pub fn pair_show(&self) -> (String, Vec<String>) {
        let code = new_pair_code();
        *self.pairing.lock().unwrap() =
            Some(Pairing { code: code.clone(), expires: Instant::now() + PAIR_WINDOW, attempts: 0 });
        (format_code(&code), sys::local_addrs())
    }

    pub fn pair_stop(&self) {
        *self.pairing.lock().unwrap() = None;
    }

    /// Sends `text` (or the current clipboard) to all paired devices. Blocking.
    pub fn send_clip_now(&self, text: Option<String>) -> Result<String, String> {
        let text = match text {
            Some(t) => t,
            None if !self.has_clip_handler() => {
                sys::clipboard_get().unwrap_or_else(|| self.last_clip.lock().unwrap().clone())
            }
            None => self.last_clip.lock().unwrap().clone(),
        };
        if text.is_empty() {
            return Err("the clipboard is empty".into());
        }
        *self.last_clip.lock().unwrap() = text.clone();
        let peers = self.peers.lock().unwrap().clone();
        if peers.is_empty() {
            return Err("no paired devices yet - pair one first (ferry pair)".into());
        }
        let mut ok = Vec::new();
        let mut errs = Vec::new();
        for p in &peers {
            match send_clip_to(self, p, &text) {
                Ok(_) => ok.push(p.name.clone()),
                Err(e) => errs.push(format!("{}: {}", p.name, e)),
            }
        }
        if ok.is_empty() {
            let e = errs.join("; ");
            self.notify("Clipboard not sent", &e);
            return Err(e);
        }
        Ok(format!("clipboard sent to {}", ok.join(", ")))
    }

    /// Sends files or text to a device (empty selector = main device). Blocking for the
    /// first attempt; if the device is not reachable the send is queued.
    pub fn send_to(self: &Arc<Self>, sel: &str, payload: Payload) -> Result<String, String> {
        if let Payload::Files(paths) = &payload {
            if paths.is_empty() {
                return Err("no files given".into());
            }
            for p in paths {
                match fs::metadata(p) {
                    Ok(m) if m.is_dir() => {
                        return Err(format!("{} is a folder - folders are not supported, zip it first", p.display()))
                    }
                    Ok(_) => {}
                    Err(e) => return Err(format!("{}: {}", p.display(), e)),
                }
            }
        }
        let dev = self.find_target(sel)?;
        let mut job = Job {
            id: {
                let mut n = self.next_job.lock().unwrap();
                *n += 1;
                *n
            },
            target: dev.id,
            target_name: dev.name.clone(),
            payload,
            done: 0,
            created: Instant::now(),
            next: Instant::now(),
            attempts: 0,
        };
        match try_job(self, &mut job) {
            Ok(msg) => {
                self.notify("Sent", &msg);
                Ok(msg)
            }
            Err(SendErr::Refused(e)) => {
                self.notify("Not sent", &e);
                Err(e)
            }
            Err(SendErr::Unreachable(e)) => {
                eprintln!("ferry: {} unreachable ({}), queued", dev.name, e);
                let msg = format!(
                    "{} is not reachable right now - queued. Ferry sends it as soon as {} is back (up to 24 h).",
                    dev.name, dev.name
                );
                job.attempts = 1;
                job.next = Instant::now() + Duration::from_secs(RETRY_STEPS[0]);
                self.outbox.0.lock().unwrap().push(job);
                self.outbox.1.notify_all();
                self.broadcast(&["queue"]);
                self.notify(&format!("Waiting for {}", dev.name), &msg);
                Ok(msg)
            }
        }
    }

    /// Retry queued sends for a device now (it just showed up).
    fn kick(&self, id: &[u8; 16]) {
        let mut g = self.outbox.0.lock().unwrap();
        let mut any = false;
        for j in g.iter_mut().filter(|j| &j.target == id) {
            j.next = Instant::now();
            any = true;
        }
        if any {
            self.outbox.1.notify_all();
        }
    }

    pub fn queue_list(&self) -> Vec<(u64, String, String, u32)> {
        self.outbox
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|j| (j.id, j.target_name.clone(), j.payload.describe(), j.attempts))
            .collect()
    }

    pub fn queue_cancel(&self, sel: &str) -> Result<String, String> {
        let mut g = self.outbox.0.lock().unwrap();
        let before = g.len();
        g.retain(|j| !(sel == "all" || j.id.to_string() == sel));
        let n = before - g.len();
        drop(g);
        self.broadcast(&["queue"]);
        if n == 0 {
            Err("nothing queued with that id".into())
        } else {
            Ok(format!("cancelled {} queued send(s)", n))
        }
    }

    // ------------------------------------------------------------ Incoming

    pub fn incoming(&self) -> Vec<Pending> {
        self.inbox.list()
    }

    fn take_incoming(&self, sel: &str) -> Vec<Pending> {
        if sel == "all" {
            self.inbox.take_all()
        } else {
            self.inbox.take(sel).into_iter().collect()
        }
    }

    pub fn accept(&self, sel: &str) -> Result<String, String> {
        let items = self.take_incoming(sel);
        if items.is_empty() {
            return Err("no such incoming transfer".into());
        }
        let dir = self.download_dir();
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut saved: Vec<PathBuf> = Vec::new();
        let mut text = None;
        for p in &items {
            for (name, _) in &p.files {
                let src = p.dir.join(name);
                let dest = unique_path(&dir, name);
                if fs::rename(&src, &dest).is_err() {
                    // different drive: copy instead
                    if let Err(e) = fs::copy(&src, &dest) {
                        eprintln!("ferry: cannot save {}: {}", name, e);
                        continue;
                    }
                }
                saved.push(dest);
            }
            if p.text.is_some() {
                text = p.text.clone();
            }
            Inbox::discard(p);
        }
        if let Some(t) = &text {
            self.set_desktop_clipboard(t);
        }
        self.broadcast(&["incoming"]);
        let mut parts = Vec::new();
        if !saved.is_empty() {
            parts.push(if saved.len() == 1 {
                format!("saved {}", saved[0].display())
            } else {
                format!("saved {} files to {}", saved.len(), dir.display())
            });
        }
        if text.is_some() {
            parts.push("text copied to the clipboard".into());
        }
        let msg = parts.join(", ");
        if let Some(first) = saved.first() {
            let open = if saved.len() == 1 { first.clone() } else { dir.clone() };
            self.notify_file("Accepted", &msg, &open);
        }
        Ok(msg)
    }

    pub fn decline(&self, sel: &str) -> Result<String, String> {
        let items = self.take_incoming(sel);
        if items.is_empty() {
            return Err("no such incoming transfer".into());
        }
        for p in &items {
            Inbox::discard(p);
        }
        self.broadcast(&["incoming"]);
        Ok(format!("declined {} transfer(s)", items.len()))
    }

    /// Blocks the sender of an incoming transfer, or a nearby device by name/id.
    pub fn block(&self, sel: &str) -> Result<String, String> {
        let from_incoming = self.inbox.list().into_iter().find(|p| !sel.is_empty() && p.id.starts_with(sel));
        let (id, ip, name) = match from_incoming {
            Some(p) => (p.from_id, p.ip.clone(), p.from_name.clone()),
            None => {
                let d = self.find_target(sel)?;
                if d.paired {
                    return Err(format!("{} is paired - use `ferry unpair` instead", d.name));
                }
                let ip = d.addr.rsplit_once(':').map(|(h, _)| h.trim_matches(['[', ']']).to_string()).unwrap_or_default();
                (d.id, ip, d.name)
            }
        };
        {
            let mut b = self.blocked.lock().unwrap();
            b.retain(|x| x.id != id);
            b.push(Blocked { id, ip, name: name.clone() });
            let _ = save_blocked(&b);
        }
        for i in self.inbox.ids_from(&id) {
            let _ = self.decline(&i);
        }
        self.nearby.lock().unwrap().remove(&id);
        Ok(format!("blocked {} - it can no longer send you anything", name))
    }

    pub fn unblock_all(&self) -> String {
        let mut b = self.blocked.lock().unwrap();
        let n = b.len();
        b.clear();
        let _ = save_blocked(&b);
        format!("unblocked {} device(s)", n)
    }

    /// Shows the (non-intrusive) notification for a new incoming transfer.
    fn announce_incoming(self: &Arc<Self>, p: &Pending) {
        self.broadcast(&["incoming"]);
        if !self.cfg.lock().unwrap().notifications {
            return;
        }
        let title = format!("{} wants to send you {}", p.from_name, p.summary());
        let body = format!(
            "{} is not one of your paired devices. Accept to keep it - otherwise it is deleted in {} h.",
            p.from_name,
            self.cfg.lock().unwrap().incoming_hours
        );
        let st = self.clone();
        let id = p.id.clone();
        sys::notify_incoming(
            &title,
            &body,
            Box::new(move |choice: &str| {
                let r = match choice {
                    "accept" => st.accept(&id),
                    "decline" => st.decline(&id),
                    _ => return,
                };
                if let Err(e) = r {
                    eprintln!("ferry: {}", e);
                }
            }),
        );
    }
}

// ---------------------------------------------------------------- outgoing

fn resolve(addr: &str, default_port: u16) -> io::Result<Vec<SocketAddr>> {
    let addr = addr.trim();
    if let Ok(sa) = addr.parse::<SocketAddr>() {
        return Ok(vec![sa]);
    }
    let has_port = if addr.starts_with('[') { addr.contains("]:") } else { addr.matches(':').count() == 1 };
    let a = if has_port { addr.to_string() } else { format!("{}:{}", addr, default_port) };
    a.to_socket_addrs().map(|i| i.collect()).map_err(|e| unreachable(format!("cannot resolve {}: {}", addr, e)))
}

fn tcp_connect(addr: &str, default_port: u16) -> io::Result<(TcpStream, SocketAddr)> {
    let mut last = unreachable(format!("{}: no address", addr));
    for sa in resolve(addr, default_port)? {
        match TcpStream::connect_timeout(&sa, CONNECT_TIMEOUT) {
            Ok(s) => {
                s.set_read_timeout(Some(IO_TIMEOUT))?;
                s.set_write_timeout(Some(IO_TIMEOUT))?;
                let _ = s.set_nodelay(true);
                return Ok((s, sa));
            }
            Err(e) => last = unreachable(format!("{}: {}", sa, e)),
        }
    }
    Err(last)
}

/// Broadcast a (v0.1-compatible) lookup for a paired device and wait briefly for it.
fn discover(st: &State, peer: &Peer) -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.set_broadcast(true).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(400))).ok()?;
    let q = disc_packet(DISC_QUERY, &st.id, st.port());
    let port = peer
        .addr
        .as_ref()
        .and_then(|a| a.rsplit(':').next()?.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let deadline = Instant::now() + Duration::from_millis(1600);
    let mut buf = [0u8; 128];
    while Instant::now() < deadline {
        for bp in broadcast_ports(port) {
            let _ = s.send_to(&q, ("255.255.255.255", bp));
        }
        while let Ok((n, from)) = s.recv_from(&mut buf) {
            if let Some((m, id, p)) = parse_disc(&buf[..n]) {
                if &m == DISC_ANSWER && id == peer.id {
                    return Some(format!("{}:{}", from.ip(), p));
                }
            }
        }
    }
    None
}

/// Opens an authenticated session to a paired peer (HELLO already exchanged).
fn open_session(st: &State, peer: &Peer) -> io::Result<Channel> {
    let mut stream = None;
    if let Some(a) = &peer.addr {
        if let Ok((s, _)) = tcp_connect(a, DEFAULT_PORT) {
            stream = Some((s, a.clone()));
        }
    }
    if stream.is_none() {
        // fresh address from a presence answer, else a broadcast lookup
        let near = st
            .nearby
            .lock()
            .unwrap()
            .get(&peer.id)
            .filter(|n| n.seen.elapsed() < NEARBY_FRESH)
            .map(|n| n.addr.clone());
        if let Some(a) = near.or_else(|| discover(st, peer)) {
            if let Ok((s, _)) = tcp_connect(&a, DEFAULT_PORT) {
                stream = Some((s, a));
            }
        }
    }
    let Some((s, used)) = stream else {
        return Err(unreachable(format!(
            "{} is not reachable (is Ferry running on it and on the same network?)",
            peer.name
        )));
    };
    let mut e = client_handshake(s, &st.id, ClientAuth::Session(&peer.key))?;
    if e.peer_id != peer.id {
        return err("connected to the wrong device");
    }
    e.ch.send(hello_msg(&st.name(), st.port()))?;
    let h = parse_hello(&e.ch.expect(T_HELLO)?)?;
    st.touch_peer(&peer.id, &h.name, Some(used), h.kind);
    Ok(e.ch)
}

/// Opens an unauthenticated guest session to a nearby (unpaired) device.
fn open_guest(st: &State, id: &[u8; 16], name: &str) -> io::Result<Channel> {
    let fresh = |st: &State| {
        st.nearby.lock().unwrap().get(id).filter(|n| n.seen.elapsed() < NEARBY_FRESH).map(|n| n.addr.clone())
    };
    let addr = match fresh(st) {
        Some(a) => a,
        None => {
            st.scan();
            fresh(st).ok_or_else(|| unreachable(format!("{} is not on the network right now", name)))?
        }
    };
    let (s, _) = tcp_connect(&addr, DEFAULT_PORT)?;
    let mut e = client_handshake(s, &st.id, ClientAuth::Guest)?;
    if &e.peer_id != id {
        st.nearby.lock().unwrap().remove(id);
        return Err(unreachable(format!("{} moved to a different address", name)));
    }
    e.ch.send(hello_msg(&st.name(), st.port()))?;
    parse_hello(&e.ch.expect(T_HELLO)?)?;
    Ok(e.ch)
}

fn send_clip_to(st: &State, peer: &Peer, text: &str) -> io::Result<()> {
    let mut ch = open_session(st, peer)?;
    ch.send(Writer::new(T_CLIP).str(text))?;
    ch.wait_ack()?;
    let _ = ch.send(Writer::new(T_BYE));
    Ok(())
}

fn send_file(ch: &mut Channel, p: &Path) -> io::Result<()> {
    let mut f = fs::File::open(p)
        .map_err(|e| io::Error::new(io::ErrorKind::NotFound, format!("{}: {}", p.display(), e)))?;
    let size = f.metadata()?.len();
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    ch.send(Writer::new(T_FILE).str(&name).u64(size))?;
    let mut buf = vec![0u8; CHUNK];
    let mut left = size;
    while left > 0 {
        let want = (left as usize).min(CHUNK);
        f.read_exact(&mut buf[..want])?;
        let mut msg = Vec::with_capacity(want + 1);
        msg.push(T_DATA);
        msg.extend_from_slice(&buf[..want]);
        ch.send_raw(msg)?;
        left -= want as u64;
    }
    ch.wait_ack()
}

/// One delivery attempt for a job. Paired devices save files as they arrive, so a
/// retry continues after the last delivered file; guests get everything again.
fn try_job(st: &State, job: &mut Job) -> Result<String, SendErr> {
    if let Some(peer) = st.paired(&job.target) {
        let mut ch = open_session(st, &peer)?;
        match &job.payload {
            Payload::Text(t) => {
                ch.send(Writer::new(T_CLIP).str(t))?;
                ch.wait_ack()?;
            }
            Payload::Files(paths) => {
                while job.done < paths.len() {
                    send_file(&mut ch, &paths[job.done])?;
                    job.done += 1;
                }
            }
        }
        let _ = ch.send(Writer::new(T_BYE));
        return Ok(format!("Sent {} to {}", job.payload.describe(), peer.name));
    }
    let mut ch = open_guest(st, &job.target, &job.target_name)?;
    let (count, total, text) = match &job.payload {
        Payload::Files(p) => {
            let mut t = 0u64;
            for f in p {
                t += fs::metadata(f)
                    .map(|m| m.len())
                    .map_err(|e| SendErr::Refused(format!("{}: {}", f.display(), e)))?;
            }
            (p.len() as u32, t, 0u8)
        }
        Payload::Text(_) => (0, 0, 1),
    };
    ch.send(Writer::new(T_OFFER).u32(count).u64(total).u8(text))?;
    ch.wait_ack()?;
    match &job.payload {
        Payload::Text(t) => {
            ch.send(Writer::new(T_CLIP).str(t))?;
            ch.wait_ack()?;
        }
        Payload::Files(paths) => {
            for p in paths {
                send_file(&mut ch, p)?;
            }
        }
    }
    ch.send(Writer::new(T_BYE))?;
    Ok(format!(
        "Sent {} to {} - it waits there until they accept it",
        job.payload.describe(),
        job.target_name
    ))
}

/// Retries queued sends: when their device shows up (kick) or on a backoff timer.
fn outbox_loop(st: Arc<State>) {
    let (m, cv) = &st.outbox;
    let mut g = m.lock().unwrap();
    loop {
        let now = Instant::now();
        if let Some(i) = g.iter().position(|j| j.next <= now) {
            let mut job = g.remove(i);
            drop(g);
            match try_job(&st, &mut job) {
                Ok(msg) => st.notify("Sent", &msg),
                Err(SendErr::Refused(e)) => st.notify(&format!("Not sent to {}", job.target_name), &e),
                Err(SendErr::Unreachable(_)) if job.created.elapsed() > QUEUE_MAX_AGE => st.notify(
                    "Not sent",
                    &format!(
                        "Gave up sending {} to {} - it was not reachable for 24 h.",
                        job.payload.describe(),
                        job.target_name
                    ),
                ),
                Err(SendErr::Unreachable(_)) => {
                    let step = RETRY_STEPS[(job.attempts as usize).min(RETRY_STEPS.len() - 1)];
                    job.attempts += 1;
                    job.next = Instant::now() + Duration::from_secs(step);
                    m.lock().unwrap().push(job);
                    g = m.lock().unwrap();
                    continue;
                }
            }
            st.broadcast(&["queue"]);
            g = m.lock().unwrap();
            continue;
        }
        g = match g.iter().map(|j| j.next).min() {
            Some(t) => cv.wait_timeout(g, t.saturating_duration_since(now)).unwrap().0,
            None => cv.wait(g).unwrap(),
        };
    }
}

fn pair_connect(st: &State, addr: &str, code: &str) -> io::Result<Peer> {
    let (s, sa) = tcp_connect(addr, DEFAULT_PORT)?;
    let mut e = client_handshake(s, &st.id, ClientAuth::Pair(code))?;
    e.ch.send(hello_msg(&st.name(), st.port()))?;
    let h = parse_hello(&e.ch.expect(T_HELLO)?)?;
    let _ = e.ch.send(Writer::new(T_BYE));
    let peer = Peer { id: e.peer_id, name: h.name, key: e.new_key.unwrap(), addr: Some(sa.to_string()), kind: h.kind };
    st.upsert_peer(peer.clone());
    Ok(peer)
}

/// Tell paired devices and the network where we are now (start-up).
fn announce_all(st: &Arc<State>) {
    if st.visible() {
        st.send_presence(PRES_HERE);
    }
    let peers = st.peers.lock().unwrap().clone();
    for p in peers {
        let st = st.clone();
        std::thread::spawn(move || {
            if p.addr.is_none() {
                return;
            }
            if let Ok(mut ch) = open_session(&st, &p) {
                let _ = ch.send(Writer::new(T_BYE));
            }
        });
    }
}

// ---------------------------------------------------------------- incoming

fn sanitize_name(n: &str) -> String {
    let base = n.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base.chars().filter(|c| !c.is_control()).collect();
    s = s.trim().to_string();
    if s.is_empty() || s == "." || s == ".." || s == "meta" || s == "meta.tmp" {
        s = format!("received-{}", s.trim_matches('.'));
    }
    #[cfg(win)]
    {
        // Windows forbids <>:"/\|?*, trailing dots/spaces and device names like CON or NUL.
        s = s.chars().map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
        s = s.trim_end_matches(['.', ' ']).to_string();
        let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
            || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4);
        if reserved || s.is_empty() {
            s = format!("_{}", s);
        }
    }
    while s.len() > 200 {
        s.pop();
    }
    s
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for i in 1.. {
        let p = dir.join(format!("{} ({}){}", stem, i, ext));
        if !p.exists() {
            return p;
        }
    }
    unreachable!()
}

fn receive_file(ch: &mut Channel, dir: &Path, name: &str, size: u64) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.ferry-part", name));
    let res = (|| -> io::Result<()> {
        let mut w = BufWriter::with_capacity(256 * 1024, fs::File::create(&tmp)?);
        let mut got = 0u64;
        while got < size {
            let (t, b) = ch.recv()?;
            if t != T_DATA {
                return err("unexpected message during file transfer");
            }
            got += b.len() as u64;
            if got > size {
                return err("peer sent more data than announced");
            }
            w.write_all(&b)?;
        }
        w.flush()?;
        Ok(())
    })();
    if let Err(e) = res {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    let dest = unique_path(dir, name);
    fs::rename(&tmp, &dest)?;
    Ok(dest)
}

fn next_msg(ch: &mut Channel) -> io::Result<Option<(u8, Vec<u8>)>> {
    match ch.recv() {
        Ok(x) => Ok(Some(x)),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

/// A transfer from an unpaired device: everything goes to Incoming.
fn handle_guest(st: &Arc<State>, mut ch: Channel, from: [u8; 16], name: &str, ip: &str) -> io::Result<()> {
    let ob = ch.expect(T_OFFER)?;
    let mut r = Reader::new(&ob);
    let (count, total, _has_text) = (r.u32()?, r.u64()?, r.u8()?);
    let limit = st.cfg.lock().unwrap().incoming_limit_mb * 1_000_000;
    let used = st.inbox.pending_bytes();
    if used + total > limit {
        ch.ack(
            false,
            &format!(
                "too large - {} accepts up to {} from unpaired devices right now",
                st.name(),
                inbox::human(limit.saturating_sub(used))
            ),
        )?;
        return err("offer too large");
    }
    ch.ack(true, "")?;
    let mut p = st.inbox.begin(name, from, ip)?;
    let res = (|| -> io::Result<()> {
        let mut got = 0u64;
        while let Some((t, body)) = next_msg(&mut ch)? {
            match t {
                T_FILE => {
                    let mut r = Reader::new(&body);
                    let fname = sanitize_name(&r.str()?);
                    let size = r.u64()?;
                    got += size;
                    if got > total || p.files.len() as u32 >= count {
                        let _ = ch.ack(false, "more than offered");
                        return err("guest sent more than offered");
                    }
                    let path = receive_file(&mut ch, &p.dir, &fname, size)?;
                    let stored = path.file_name().unwrap().to_string_lossy().to_string();
                    p.files.push((stored, size));
                    ch.ack(true, "")?;
                }
                T_CLIP => {
                    p.text = Some(Reader::new(&body).str()?);
                    ch.ack(true, "")?;
                }
                T_BYE => break,
                T_HELLO => {}
                _ => return err(format!("unknown message type {}", t)),
            }
        }
        Ok(())
    })();
    match res {
        Ok(()) if !p.files.is_empty() || p.text.is_some() => {
            st.inbox.commit(p.clone())?;
            eprintln!("ferry: incoming from unpaired {} ({}): {}", name, ip, p.summary());
            st.announce_incoming(&p);
            Ok(())
        }
        Ok(()) => {
            Inbox::discard(&p);
            Ok(())
        }
        Err(e) => {
            Inbox::discard(&p);
            Err(e)
        }
    }
}

fn handle_conn(st: Arc<State>, stream: TcpStream, remote: SocketAddr) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let _ = stream.set_nodelay(true);
    let e = server_handshake(stream, &st.id, &*st)?;
    let mut ch = e.ch;
    let h = parse_hello(&ch.expect(T_HELLO)?)?;
    ch.send(hello_msg(&st.name(), st.port()))?;
    if e.guest {
        let name = if h.name.trim().is_empty() { "Unknown device".to_string() } else { h.name.clone() };
        return handle_guest(&st, ch, e.peer_id, &name, &remote.ip().to_string());
    }
    let addr = format!("{}:{}", remote.ip(), h.port);
    let peer_name = h.name.clone();
    if let Some(key) = e.new_key {
        st.upsert_peer(Peer { id: e.peer_id, name: h.name.clone(), key, addr: Some(addr), kind: h.kind });
        *st.pairing.lock().unwrap() = None;
        st.broadcast(&["paired", &h.name]);
        st.notify("Paired", &format!("Ferry is now connected to {}.", h.name));
        eprintln!("ferry: paired with {}", h.name);
    } else {
        st.touch_peer(&e.peer_id, &h.name, Some(addr), h.kind);
        st.kick(&e.peer_id);
    }

    let mut received: Vec<PathBuf> = Vec::new();
    let result = loop {
        let (t, body) = match next_msg(&mut ch) {
            Ok(Some(x)) => x,
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        };
        match t {
            T_CLIP => {
                let text = Reader::new(&body).str()?;
                let new = *st.last_clip.lock().unwrap() != text;
                if new {
                    st.set_desktop_clipboard(&text);
                    // pass it on to my other devices
                    st.queue_clip(text, Some(e.peer_id));
                }
                ch.ack(true, "")?;
            }
            T_FILE => {
                let mut r = Reader::new(&body);
                let name = sanitize_name(&r.str()?);
                let size = r.u64()?;
                let dir = st.download_dir();
                match receive_file(&mut ch, &dir, &name, size) {
                    Ok(p) => {
                        ch.ack(true, "")?;
                        received.push(p);
                    }
                    Err(e) => {
                        let _ = ch.ack(false, &e.to_string());
                        break Err(e);
                    }
                }
            }
            T_BYE => break Ok(()),
            T_HELLO => {}
            _ => break err(format!("unknown message type {}", t)),
        }
    };
    if !received.is_empty() {
        let body = if received.len() == 1 {
            format!("{}", received[0].display())
        } else {
            format!("{} files in {}", received.len(), received[0].parent().unwrap().display())
        };
        let open = if received.len() == 1 { received[0].clone() } else { received[0].parent().unwrap().to_path_buf() };
        st.notify_file(&format!("Received from {}", peer_name), &body, &open);
        for p in &received {
            st.broadcast(&["received", &p.to_string_lossy()]);
        }
    }
    result
}

fn udp_loop(st: Arc<State>, sock: UdpSocket) {
    let mut buf = [0u8; 256];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        let b = &buf[..n];
        if let Some((m, id, _)) = parse_disc(b) {
            if &m == DISC_QUERY && id != st.id && st.peer_key(&id).is_some() {
                let _ = sock.send_to(&disc_packet(DISC_ANSWER, &st.id, st.port()), from);
                continue;
            }
        }
        let Some(p) = parse_presence(b) else { continue };
        if p.id == st.id {
            continue;
        }
        let paired = st.peer_key(&p.id).is_some();
        let blocked = st.blocked.lock().unwrap().iter().any(|x| x.id == p.id);
        if !blocked {
            st.nearby.lock().unwrap().insert(
                p.id,
                Nearby {
                    id: p.id,
                    name: p.name.clone(),
                    kind: p.kind,
                    addr: format!("{}:{}", from.ip(), p.port),
                    seen: Instant::now(),
                    accepts: p.flags & PRES_GUESTS != 0,
                },
            );
        }
        if &p.magic == PRES_QUERY && (st.visible() || paired) {
            let _ = sock.send_to(&st.presence(PRES_HERE), from);
        }
        st.kick(&p.id);
    }
}

fn clip_sender(st: Arc<State>) {
    loop {
        let (text, from) = {
            let (m, cv) = &st.clip_queue;
            let mut g = m.lock().unwrap();
            while g.is_none() {
                g = cv.wait(g).unwrap();
            }
            g.take().unwrap()
        };
        let peers = st.peers.lock().unwrap().clone();
        for p in peers.iter().filter(|p| Some(p.id) != from) {
            if let Err(e) = send_clip_to(&st, p, &text) {
                eprintln!("ferry: clipboard sync to {} failed: {}", p.name, e);
            }
        }
    }
}

// ---------------------------------------------------------------- control socket

fn reply(w: &Sub, fields: &[&str]) {
    let _ = w.lock().unwrap().write_all(ipc::line(fields).as_bytes());
}

fn reply_result(w: &Sub, r: Result<String, String>) {
    match r {
        Ok(m) => reply(w, &["ok", &m]),
        Err(e) => reply(w, &["err", &e]),
    }
}

pub fn kind_str(k: u8) -> &'static str {
    match k {
        KIND_PHONE => "phone",
        KIND_PC => "pc",
        _ => "device",
    }
}

fn handle_ipc(st: Arc<State>, s: ipc::Stream) {
    let w: Sub = Arc::new(Mutex::new(match s.try_clone() {
        Ok(x) => x,
        Err(_) => return,
    }));
    let mut r = BufReader::new(s);
    let mut l = String::new();
    loop {
        l.clear();
        match r.read_line(&mut l) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let f = ipc::split(&l);
        let arg = |i: usize| f.get(i).cloned().unwrap_or_default();
        // Commands that may take a while run on their own thread.
        let bg = |job: Box<dyn FnOnce() -> Result<String, String> + Send>| {
            let w = w.clone();
            std::thread::spawn(move || reply_result(&w, job()));
        };
        match f[0].as_str() {
            "status" => {
                let c = st.cfg.lock().unwrap().clone();
                let peers = st.peers.lock().unwrap().clone();
                let addrs = sys::local_addrs().join(",");
                reply(
                    &w,
                    &[
                        "status",
                        &c.name,
                        &c.port.to_string(),
                        &c.download_dir.to_string_lossy(),
                        if c.auto_clipboard { "1" } else { "0" },
                        &addrs,
                        &to_hex(&st.id),
                        if c.visible { "1" } else { "0" },
                        &st.inbox.list().len().to_string(),
                        &st.outbox.0.lock().unwrap().len().to_string(),
                    ],
                );
                for p in peers {
                    reply(&w, &["peer", &to_hex(&p.id), &p.name, &p.addr.unwrap_or_default(), kind_str(p.kind)]);
                }
                reply(&w, &["end"]);
            }
            "devices" => {
                let st = st.clone();
                let w = w.clone();
                let scan = arg(1) != "cached";
                std::thread::spawn(move || {
                    for d in st.devices(scan) {
                        reply(
                            &w,
                            &[
                                "dev",
                                &to_hex(&d.id),
                                &d.name,
                                kind_str(d.kind),
                                if d.paired { "paired" } else { "nearby" },
                                if d.online { "1" } else { "0" },
                                &d.addr,
                            ],
                        );
                    }
                    reply(&w, &["devend"]);
                });
            }
            "subscribe" => {
                st.subs.lock().unwrap().push((w.clone(), arg(1) == "clipboard"));
                reply(&w, &["ok", "subscribed"]);
            }
            "clipchanged" => st.clipboard_changed(arg(1)),
            "sendclip" => {
                let st = st.clone();
                let text = if f.len() > 1 { Some(arg(1)) } else { None };
                bg(Box::new(move || st.send_clip_now(text)));
            }
            "sendfiles" => {
                let st = st.clone();
                let sel = arg(1);
                let paths: Vec<PathBuf> = f.iter().skip(2).filter(|p| !p.is_empty()).map(PathBuf::from).collect();
                bg(Box::new(move || st.send_to(&sel, Payload::Files(paths))));
            }
            "sendtext" => {
                let st = st.clone();
                let (sel, text) = (arg(1), arg(2));
                bg(Box::new(move || st.send_to(&sel, Payload::Text(text))));
            }
            "incoming" => {
                for p in st.incoming() {
                    let age = inbox::now_secs().saturating_sub(p.time);
                    reply(
                        &w,
                        &[
                            "in",
                            &p.id,
                            &p.from_name,
                            &p.summary(),
                            &p.files.len().to_string(),
                            &p.bytes().to_string(),
                            if p.text.is_some() { "1" } else { "0" },
                            &age.to_string(),
                        ],
                    );
                }
                reply(&w, &["inend"]);
            }
            "accept" => reply_result(&w, st.accept(&arg(1))),
            "decline" => reply_result(&w, st.decline(&arg(1))),
            "block" => {
                let st = st.clone();
                let sel = arg(1);
                bg(Box::new(move || st.block(&sel)));
            }
            "unblock" => reply(&w, &["ok", &st.unblock_all()]),
            "queue" => {
                for (id, name, what, attempts) in st.queue_list() {
                    reply(&w, &["job", &id.to_string(), &name, &what, &attempts.to_string()]);
                }
                reply(&w, &["jobend"]);
            }
            "cancel" => reply_result(&w, st.queue_cancel(&arg(1))),
            "pairshow" => {
                let (code, addrs) = st.pair_show();
                reply(&w, &["pair", &code, &st.port().to_string(), &addrs.join(","), &st.name()]);
            }
            "pairstop" => {
                st.pair_stop();
                reply(&w, &["ok", "pairing stopped"]);
            }
            "pairconnect" => {
                let st = st.clone();
                let (addr, code) = (arg(1), arg(2));
                bg(Box::new(move || {
                    pair_connect(&st, &addr, &code)
                        .map(|p| {
                            st.broadcast(&["paired", &p.name]);
                            format!("paired with {}", p.name)
                        })
                        .map_err(|e| e.to_string())
                }));
            }
            "unpair" => {
                let sel = arg(1).to_lowercase();
                let found = st
                    .peers
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|p| {
                        !sel.is_empty() && (p.name.to_lowercase().starts_with(&sel) || to_hex(&p.id).starts_with(&sel))
                    })
                    .cloned();
                match found {
                    Some(p) => {
                        st.peers.lock().unwrap().retain(|x| x.id != p.id);
                        st.save_peers();
                        st.broadcast(&["devices"]);
                        reply(&w, &["ok", &format!("removed {}", p.name)]);
                    }
                    None => reply(&w, &["err", "no paired device with that name"]),
                }
            }
            "set" => reply_result(&w, st.set_option(&arg(1), &arg(2))),
            "announce" => {
                announce_all(&st);
                reply(&w, &["ok", "announced"]);
            }
            "" => {}
            other => reply(&w, &["err", &format!("unknown command '{}'", other)]),
        }
    }
    st.subs.lock().unwrap().retain(|(s, _)| !Arc::ptr_eq(s, &w));
}

// ---------------------------------------------------------------- entry point

pub fn run() -> io::Result<()> {
    let cfg = Config::load();
    let ipc_l = ipc::Listener::bind()?;

    let tcp = TcpListener::bind(("0.0.0.0", cfg.port))
        .or_else(|e| err(format!("cannot listen on TCP port {}: {}", cfg.port, e)))?;
    let udp = UdpSocket::bind(("0.0.0.0", cfg.port)).and_then(|u| {
        u.set_broadcast(true)?;
        Ok(u)
    });
    let (udp_rx, udp_tx) = match udp {
        Ok(u) => {
            let tx = u.try_clone().ok();
            (Some(u), tx)
        }
        Err(e) => {
            eprintln!("ferry: discovery disabled (UDP {}): {}", cfg.port, e);
            (None, None)
        }
    };

    let st = Arc::new(State {
        id: load_identity(),
        peers: Mutex::new(load_peers()),
        cfg: Mutex::new(cfg.clone()),
        pairing: Mutex::new(None),
        subs: Mutex::new(Vec::new()),
        last_clip: Mutex::new(String::new()),
        clip_queue: (Mutex::new(None), Condvar::new()),
        nearby: Mutex::new(HashMap::new()),
        udp: udp_tx,
        inbox: Inbox::load(config_dir().join("incoming")),
        outbox: (Mutex::new(Vec::new()), Condvar::new()),
        next_job: Mutex::new(0),
        blocked: Mutex::new(load_blocked()),
        guest_rate: Mutex::new(HashMap::new()),
    });
    eprintln!(
        "ferry: '{}' listening on port {} ({} paired device(s), {} waiting in Incoming), id {}",
        cfg.name,
        cfg.port,
        st.peers.lock().unwrap().len(),
        st.inbox.list().len(),
        to_hex(&st.id)
    );

    if let Some(u) = udp_rx {
        let st2 = st.clone();
        std::thread::spawn(move || udp_loop(st2, u));
    }
    for f in [clip_sender as fn(Arc<State>), outbox_loop] {
        let st2 = st.clone();
        std::thread::spawn(move || f(st2));
    }
    {
        let st2 = st.clone();
        std::thread::spawn(move || {
            let st3 = st2.clone();
            let st4 = st2.clone();
            st2.inbox.expiry_loop(
                move || st3.cfg.lock().unwrap().incoming_hours,
                move || st4.broadcast(&["incoming"]),
            );
        });
    }
    #[cfg(not(win))]
    if !sys::is_gnome() {
        let st2 = st.clone();
        std::thread::spawn(move || {
            sys::watch_clipboard_wl(|| {
                if sys::clipboard_is_secret() {
                    return;
                }
                if let Some(t) = sys::clipboard_get() {
                    st2.clipboard_changed(t);
                }
            })
        });
    }
    {
        let st2 = st.clone();
        std::thread::spawn(move || loop {
            if let Ok(s) = ipc_l.accept() {
                let st3 = st2.clone();
                std::thread::spawn(move || handle_ipc(st3, s));
            }
        });
    }
    announce_all(&st);
    {
        let st2 = st.clone();
        std::thread::spawn(move || tcp_loop(st2, tcp));
    }
    // Linux: just waits. Windows: runs the tray icon / clipboard listener on this thread.
    sys::main_loop(st)
}

fn tcp_loop(st: Arc<State>, tcp: TcpListener) {
    for conn in tcp.incoming() {
        match conn {
            Ok(s) => {
                let st2 = st.clone();
                let remote = s.peer_addr().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());
                std::thread::spawn(move || {
                    if let Err(e) = handle_conn(st2, s, remote) {
                        eprintln!("ferry: connection from {}: {}", remote, e);
                    }
                });
            }
            Err(e) => eprintln!("ferry: accept: {}", e),
        }
    }
}
