//! The background daemon: listens for the phone, serves the local control socket.
//! Idle cost: a handful of threads blocked in accept()/recv() - no polling, no timers.

use crate::crypto::to_hex;
use crate::ipc;
use crate::proto::*;
use crate::store::*;
use crate::sys;
use std::fs;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PAIR_WINDOW: Duration = Duration::from_secs(300);
const PAIR_ATTEMPTS: u32 = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const IO_TIMEOUT: Duration = Duration::from_secs(60);

struct Pairing {
    code: String,
    expires: Instant,
    attempts: u32,
}

type Sub = Arc<Mutex<ipc::Stream>>;

pub struct State {
    id: [u8; 16],
    cfg: Mutex<Config>,
    peers: Mutex<Vec<Peer>>,
    pairing: Mutex<Option<Pairing>>,
    /// (connection, handles-clipboard)
    subs: Mutex<Vec<(Sub, bool)>>,
    /// Latest clipboard text known on this desktop (to avoid echo loops).
    last_clip: Mutex<String>,
    clip_queue: (Mutex<Option<String>>, Condvar),
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

    /// Insert or update a peer and move it to the front (= default target).
    fn upsert_peer(&self, p: Peer) {
        {
            let mut g = self.peers.lock().unwrap();
            g.retain(|x| x.id != p.id);
            g.insert(0, p);
        }
        self.save_peers();
    }

    fn touch_peer(&self, id: &[u8; 16], name: &str, addr: Option<String>) {
        let mut changed = false;
        {
            let mut g = self.peers.lock().unwrap();
            if let Some(i) = g.iter().position(|p| &p.id == id) {
                let mut p = g.remove(i);
                if p.name != name && !name.is_empty() {
                    p.name = name.to_string();
                    changed = true;
                }
                if addr.is_some() && p.addr != addr {
                    p.addr = addr;
                    changed = true;
                }
                changed |= i != 0;
                g.insert(0, p);
            }
        }
        if changed {
            self.save_peers();
        }
    }

    fn find_peer(&self, sel: &str) -> Result<Peer, String> {
        let g = self.peers.lock().unwrap();
        if g.is_empty() {
            return Err("no paired devices yet - run `ferry pair` first".into());
        }
        if sel.is_empty() {
            return Ok(g[0].clone());
        }
        let s = sel.to_lowercase();
        g.iter()
            .find(|p| p.name.to_lowercase() == s)
            .or_else(|| g.iter().find(|p| p.name.to_lowercase().starts_with(&s) || to_hex(&p.id).starts_with(&s)))
            .cloned()
            .ok_or_else(|| format!("no paired device matches '{}'", sel))
    }

    /// Sets the desktop clipboard (through the GNOME extension when available).
    fn set_desktop_clipboard(&self, text: &str) {
        *self.last_clip.lock().unwrap() = text.to_string();
        if self.has_clip_handler() {
            self.broadcast(&["setclip", text]);
        } else if !sys::clipboard_set(text) {
            eprintln!("ferry: no way to set the clipboard (install the GNOME extension or wl-clipboard/xclip)");
        }
    }

    /// Called whenever the desktop clipboard changed.
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
        if self.cfg.lock().unwrap().auto_clipboard {
            let (m, cv) = &self.clip_queue;
            *m.lock().unwrap() = Some(text);
            cv.notify_one();
        }
    }

    // ------------------------------------------------------------ API for CLI / tray

    pub fn auto_clipboard(&self) -> bool {
        self.cfg.lock().unwrap().auto_clipboard
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
        let auto = c.auto_clipboard;
        drop(c);
        self.broadcast(&["config", "auto_clipboard", if auto { "1" } else { "0" }]);
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

    /// Sends `text` (or the current clipboard) to the default device. Blocking.
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
        let r = self
            .find_peer("")
            .and_then(|p| send_clip_to(self, &p, &text).map(|_| p).map_err(|e| e.to_string()));
        match r {
            Ok(p) => Ok(format!("clipboard sent to {}", p.name)),
            Err(e) => {
                self.notify("Clipboard not sent", &e);
                Err(e)
            }
        }
    }

    /// Sends files to the device matching `sel` (empty = default). Blocking, notifies.
    pub fn send_files_now(&self, sel: &str, paths: &[PathBuf]) -> Result<String, String> {
        if paths.is_empty() {
            return Err("no files given".into());
        }
        let res = self
            .find_peer(sel)
            .and_then(|p| send_files_to(self, &p, paths).map(|_| p).map_err(|e| e.to_string()));
        match res {
            Ok(p) => {
                let msg = if paths.len() == 1 {
                    format!("Sent {} to {}", paths[0].file_name().unwrap_or_default().to_string_lossy(), p.name)
                } else {
                    format!("Sent {} files to {}", paths.len(), p.name)
                };
                self.notify("Sent", &msg);
                Ok(msg)
            }
            Err(e) => {
                self.notify("Sending failed", &e);
                Err(e)
            }
        }
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
    Ok(a.to_socket_addrs()?.collect())
}

fn tcp_connect(addr: &str, default_port: u16) -> io::Result<(TcpStream, SocketAddr)> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
    for sa in resolve(addr, default_port)? {
        match TcpStream::connect_timeout(&sa, CONNECT_TIMEOUT) {
            Ok(s) => {
                s.set_read_timeout(Some(IO_TIMEOUT))?;
                s.set_write_timeout(Some(IO_TIMEOUT))?;
                let _ = s.set_nodelay(true);
                return Ok((s, sa));
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Broadcast a discovery query and wait briefly for the peer to answer.
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
    let mut buf = [0u8; 64];
    while Instant::now() < deadline {
        let _ = s.send_to(&q, ("255.255.255.255", port));
        if port != DEFAULT_PORT {
            let _ = s.send_to(&q, ("255.255.255.255", DEFAULT_PORT));
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
        if let Some(a) = discover(st, peer) {
            if let Ok((s, _)) = tcp_connect(&a, DEFAULT_PORT) {
                stream = Some((s, a));
            }
        }
    }
    let Some((s, used)) = stream else {
        return err(format!("{} is not reachable (is Ferry running on it and on the same network?)", peer.name));
    };
    let mut e = client_handshake(s, &st.id, ClientAuth::Session(&peer.key))?;
    if e.peer_id != peer.id {
        return err("connected to the wrong device");
    }
    e.ch.send(hello_msg(&st.name(), st.port()))?;
    let h = parse_hello(&e.ch.expect(T_HELLO)?)?;
    st.touch_peer(&peer.id, &h.name, Some(used));
    Ok(e.ch)
}

fn send_clip_to(st: &State, peer: &Peer, text: &str) -> io::Result<()> {
    let mut ch = open_session(st, peer)?;
    ch.send(Writer::new(T_CLIP).str(text))?;
    ch.wait_ack()?;
    let _ = ch.send(Writer::new(T_BYE));
    Ok(())
}

fn send_files_to(st: &State, peer: &Peer, paths: &[PathBuf]) -> io::Result<()> {
    for p in paths {
        let m = fs::metadata(p).or_else(|e| err(format!("{}: {}", p.display(), e)))?;
        if m.is_dir() {
            return err(format!("{} is a folder - folders are not supported, zip it first", p.display()));
        }
    }
    let mut ch = open_session(st, peer)?;
    let mut buf = vec![0u8; CHUNK];
    for p in paths {
        let mut f = fs::File::open(p)?;
        let size = f.metadata()?.len();
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
        ch.send(Writer::new(T_FILE).str(&name).u64(size))?;
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
        ch.wait_ack()?;
    }
    let _ = ch.send(Writer::new(T_BYE));
    Ok(())
}

fn pair_connect(st: &State, addr: &str, code: &str) -> io::Result<Peer> {
    let (s, sa) = tcp_connect(addr, DEFAULT_PORT)?;
    let mut e = client_handshake(s, &st.id, ClientAuth::Pair(code))?;
    e.ch.send(hello_msg(&st.name(), st.port()))?;
    let h = parse_hello(&e.ch.expect(T_HELLO)?)?;
    let _ = e.ch.send(Writer::new(T_BYE));
    let peer = Peer { id: e.peer_id, name: h.name, key: e.new_key.unwrap(), addr: Some(sa.to_string()) };
    st.upsert_peer(peer.clone());
    Ok(peer)
}

/// Tell peers where we are now (after start-up / network change).
fn announce_all(st: &Arc<State>) {
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
    if s.is_empty() || s == "." || s == ".." {
        s = "received-file".into();
    }
    #[cfg(win)]
    {
        // Windows forbids <>:"/\\|?*, trailing dots/spaces and device names like CON or NUL.
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

fn handle_conn(st: Arc<State>, stream: TcpStream, remote: SocketAddr) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let _ = stream.set_nodelay(true);
    let e = server_handshake(stream, &st.id, &*st)?;
    let mut ch = e.ch;
    let h = parse_hello(&ch.expect(T_HELLO)?)?;
    ch.send(hello_msg(&st.name(), st.port()))?;
    let addr = format!("{}:{}", remote.ip(), h.port);
    let peer_name = h.name.clone();
    if let Some(key) = e.new_key {
        st.upsert_peer(Peer { id: e.peer_id, name: h.name.clone(), key, addr: Some(addr) });
        *st.pairing.lock().unwrap() = None;
        st.broadcast(&["paired", &h.name]);
        st.notify("Paired", &format!("Ferry is now connected to {}.", h.name));
        eprintln!("ferry: paired with {}", h.name);
    } else {
        st.touch_peer(&e.peer_id, &h.name, Some(addr));
    }

    let mut received: Vec<PathBuf> = Vec::new();
    let result = loop {
        let (t, body) = match ch.recv() {
            Ok(x) => x,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break Ok(()),
            Err(e) => break Err(e),
        };
        match t {
            T_CLIP => {
                let text = Reader::new(&body).str()?;
                st.set_desktop_clipboard(&text);
                ch.ack(true, "")?;
            }
            T_FILE => {
                let mut r = Reader::new(&body);
                let name = sanitize_name(&r.str()?);
                let size = r.u64()?;
                let dir = st.cfg.lock().unwrap().download_dir.clone();
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
    let mut buf = [0u8; 64];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        if let Some((m, id, _)) = parse_disc(&buf[..n]) {
            if &m == DISC_QUERY && id != st.id && st.peer_key(&id).is_some() {
                let _ = sock.send_to(&disc_packet(DISC_ANSWER, &st.id, st.port()), from);
            }
        }
    }
}

fn clip_sender(st: Arc<State>) {
    loop {
        let text = {
            let (m, cv) = &st.clip_queue;
            let mut g = m.lock().unwrap();
            while g.is_none() {
                g = cv.wait(g).unwrap();
            }
            g.take().unwrap()
        };
        let peers = st.peers.lock().unwrap().clone();
        for p in peers {
            if let Err(e) = send_clip_to(&st, &p, &text) {
                eprintln!("ferry: clipboard sync to {} failed: {}", p.name, e);
            }
        }
    }
}

// ---------------------------------------------------------------- control socket

fn reply(w: &Sub, fields: &[&str]) {
    let _ = w.lock().unwrap().write_all(ipc::line(fields).as_bytes());
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
                    ],
                );
                for p in peers {
                    reply(&w, &["peer", &to_hex(&p.id), &p.name, &p.addr.unwrap_or_default()]);
                }
                reply(&w, &["end"]);
            }
            "subscribe" => {
                st.subs.lock().unwrap().push((w.clone(), arg(1) == "clipboard"));
                reply(&w, &["ok", "subscribed"]);
            }
            "clipchanged" => st.clipboard_changed(arg(1)),
            "sendclip" => {
                let st = st.clone();
                let w = w.clone();
                let text = if f.len() > 1 { Some(arg(1)) } else { None };
                std::thread::spawn(move || match st.send_clip_now(text) {
                    Ok(m) => reply(&w, &["ok", &m]),
                    Err(e) => reply(&w, &["err", &e]),
                });
            }
            "sendfiles" => {
                let st = st.clone();
                let w = w.clone();
                let sel = arg(1);
                let paths: Vec<PathBuf> = f.iter().skip(2).filter(|p| !p.is_empty()).map(PathBuf::from).collect();
                std::thread::spawn(move || match st.send_files_now(&sel, &paths) {
                    Ok(m) => reply(&w, &["ok", &m]),
                    Err(e) => reply(&w, &["err", &e]),
                });
            }
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
                let w = w.clone();
                let (addr, code) = (arg(1), arg(2));
                std::thread::spawn(move || match pair_connect(&st, &addr, &code) {
                    Ok(p) => {
                        st.broadcast(&["paired", &p.name]);
                        reply(&w, &["ok", &format!("paired with {}", p.name)]);
                    }
                    Err(e) => reply(&w, &["err", &e.to_string()]),
                });
            }
            "unpair" => {
                let sel = arg(1);
                match st.find_peer(&sel) {
                    Ok(p) if !sel.is_empty() => {
                        st.peers.lock().unwrap().retain(|x| x.id != p.id);
                        st.save_peers();
                        reply(&w, &["ok", &format!("removed {}", p.name)]);
                    }
                    Ok(_) => reply(&w, &["err", "say which device to remove"]),
                    Err(e) => reply(&w, &["err", &e]),
                }
            }
            "set" => match st.set_option(&arg(1), &arg(2)) {
                Ok(m) => reply(&w, &["ok", &m]),
                Err(e) => reply(&w, &["err", &e]),
            },
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
    let udp = UdpSocket::bind(("0.0.0.0", cfg.port));

    let st = Arc::new(State {
        id: load_identity(),
        peers: Mutex::new(load_peers()),
        cfg: Mutex::new(cfg.clone()),
        pairing: Mutex::new(None),
        subs: Mutex::new(Vec::new()),
        last_clip: Mutex::new(String::new()),
        clip_queue: (Mutex::new(None), Condvar::new()),
    });
    eprintln!(
        "ferry: '{}' listening on port {} ({} paired device(s)), id {}",
        cfg.name,
        cfg.port,
        st.peers.lock().unwrap().len(),
        to_hex(&st.id)
    );

    match udp {
        Ok(u) => {
            let st2 = st.clone();
            std::thread::spawn(move || udp_loop(st2, u));
        }
        Err(e) => eprintln!("ferry: discovery disabled (UDP {}): {}", cfg.port, e),
    }
    {
        let st2 = st.clone();
        std::thread::spawn(move || clip_sender(st2));
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
