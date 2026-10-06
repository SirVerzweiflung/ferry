//! Ferry wire protocol v1 (see PROTOCOL.md).

use crate::crypto::*;
use std::io::{self, Read, Write};
use std::net::TcpStream;

pub const MAGIC: &[u8; 4] = b"FRY1";
pub const DEFAULT_PORT: u16 = 47800;
pub const MODE_SESSION: u8 = 1;
pub const MODE_PAIR: u8 = 2;
/// v0.2: unpaired ("nearby") device. Encrypted but not authenticated; the receiver
/// keeps everything in Incoming until the user accepts it.
pub const MODE_GUEST: u8 = 3;

pub const ST_OK: u8 = 0;
pub const ST_UNKNOWN_PEER: u8 = 1;
pub const ST_NOT_PAIRING: u8 = 2;
pub const ST_NO_GUESTS: u8 = 3;
pub const ST_BUSY: u8 = 4;

pub const T_HELLO: u8 = 1;
pub const T_CLIP: u8 = 2;
pub const T_FILE: u8 = 3;
pub const T_DATA: u8 = 4;
pub const T_ACK: u8 = 5;
pub const T_BYE: u8 = 6;
/// Guest sessions announce what they will send: count u32, total bytes u64, has_text u8.
pub const T_OFFER: u8 = 7;

pub const KIND_PC: u8 = 1;
pub const KIND_PHONE: u8 = 2;

pub const MAX_PLAINTEXT: usize = 1 << 20;
pub const CHUNK: usize = 64 * 1024;
pub const MAX_CLIP: usize = 1 << 19;

pub const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
pub const CODE_LEN: usize = 10;

pub fn err<T>(msg: impl Into<String>) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Other, msg.into()))
}

// ---------------------------------------------------------------- pairing code

pub fn new_pair_code() -> String {
    let mut out = String::new();
    while out.len() < CODE_LEN {
        let b: [u8; 1] = random_array();
        let n = CODE_ALPHABET.len() as u8;
        if b[0] < (256 / n as u16 * n as u16) as u8 {
            out.push(CODE_ALPHABET[(b[0] % n) as usize] as char);
        }
    }
    out
}

/// Uppercase, strip separators/whitespace.
pub fn normalize_code(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

pub fn format_code(c: &str) -> String {
    if c.len() == CODE_LEN {
        format!("{}-{}", &c[..5], &c[5..])
    } else {
        c.to_string()
    }
}

// ---------------------------------------------------------------- message encoding

#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn new(t: u8) -> Self {
        Writer(vec![t])
    }
    pub fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    pub fn u16(mut self, v: u16) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u64(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn str(mut self, s: &str) -> Self {
        self.0.extend_from_slice(&(s.len() as u32).to_be_bytes());
        self.0.extend_from_slice(s.as_bytes());
        self
    }
}

pub struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b, p: 0 }
    }
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.p + n > self.b.len() {
            return err("truncated message");
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> io::Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    pub fn u32(&mut self) -> io::Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    pub fn u64(&mut self) -> io::Result<u64> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_be_bytes(a))
    }
    pub fn str(&mut self) -> io::Result<String> {
        let n = self.u32()? as usize;
        let s = self.take(n)?;
        String::from_utf8(s.to_vec()).or_else(|_| err("invalid utf-8"))
    }
}

// ---------------------------------------------------------------- secure channel

pub struct Channel {
    pub stream: TcpStream,
    send_key: [u8; 32],
    recv_key: [u8; 32],
    send_ctr: u64,
    recv_ctr: u64,
}

fn nonce(ctr: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&ctr.to_be_bytes());
    n
}

impl Channel {
    pub fn send_raw(&mut self, plaintext: Vec<u8>) -> io::Result<()> {
        if plaintext.len() > MAX_PLAINTEXT {
            return err("frame too large");
        }
        let mut buf = Vec::with_capacity(4 + plaintext.len() + 16);
        buf.extend_from_slice(&[0u8; 4]);
        let mut body = plaintext;
        aead_seal(&self.send_key, &nonce(self.send_ctr), &[], &mut body);
        self.send_ctr += 1;
        buf[..4].copy_from_slice(&(body.len() as u32).to_be_bytes());
        buf.extend_from_slice(&body);
        self.stream.write_all(&buf)
    }

    pub fn send(&mut self, w: Writer) -> io::Result<()> {
        self.send_raw(w.0)
    }

    /// Returns (type, body).
    pub fn recv(&mut self) -> io::Result<(u8, Vec<u8>)> {
        let mut lb = [0u8; 4];
        self.stream.read_exact(&mut lb)?;
        let len = u32::from_be_bytes(lb) as usize;
        if len < 17 || len > MAX_PLAINTEXT + 16 {
            return err("bad frame length");
        }
        let mut buf = vec![0u8; len];
        self.stream.read_exact(&mut buf)?;
        if !aead_open(&self.recv_key, &nonce(self.recv_ctr), &[], &mut buf) {
            return err("authentication failed (wrong key?)");
        }
        self.recv_ctr += 1;
        let t = buf[0];
        buf.remove(0);
        Ok((t, buf))
    }

    pub fn expect(&mut self, t: u8) -> io::Result<Vec<u8>> {
        let (rt, body) = self.recv()?;
        if rt != t {
            return err(format!("unexpected message type {} (wanted {})", rt, t));
        }
        Ok(body)
    }

    pub fn ack(&mut self, ok: bool, msg: &str) -> io::Result<()> {
        self.send(Writer::new(T_ACK).u8(ok as u8).str(msg))
    }

    pub fn wait_ack(&mut self) -> io::Result<()> {
        let b = self.expect(T_ACK)?;
        let mut r = Reader::new(&b);
        let ok = r.u8()? == 1;
        let msg = r.str()?;
        if ok {
            Ok(())
        } else {
            err(format!("peer refused: {}", msg))
        }
    }
}

// ---------------------------------------------------------------- handshake

pub struct Hello {
    pub name: String,
    pub port: u16,
    pub kind: u8,
}

pub fn hello_msg(name: &str, port: u16) -> Writer {
    Writer::new(T_HELLO).str(name).u16(port).u32(KIND_PC as u32)
}

pub fn parse_hello(b: &[u8]) -> io::Result<Hello> {
    let mut r = Reader::new(b);
    let name = r.str()?;
    let port = r.u16()?;
    let kind = r.u32().map(|f| (f & 0xff) as u8).unwrap_or(0);
    Ok(Hello { name, port, kind })
}

pub struct Established {
    pub ch: Channel,
    pub peer_id: [u8; 16],
    /// Long-term key established by pairing (pair mode only).
    pub new_key: Option<[u8; 32]>,
    /// Unpaired guest session.
    pub guest: bool,
}

fn dh(sk: &[u8; 32], pk: &[u8; 32]) -> io::Result<[u8; 32]> {
    let s = x25519(sk, pk);
    if s == [0u8; 32] {
        return err("invalid public key");
    }
    Ok(s)
}

fn hello_bytes(tag: u8, id: &[u8; 16], eph: &[u8; 32]) -> [u8; 53] {
    let mut b = [0u8; 53];
    b[..4].copy_from_slice(MAGIC);
    b[4] = tag;
    b[5..21].copy_from_slice(id);
    b[21..53].copy_from_slice(eph);
    b
}

fn split_keys(okm: &[u8]) -> ([u8; 32], [u8; 32]) {
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    a.copy_from_slice(&okm[..32]);
    b.copy_from_slice(&okm[32..64]);
    (a, b)
}

pub enum ClientAuth<'a> {
    Session(&'a [u8; 32]),
    Pair(&'a str),
    Guest,
}

pub fn client_handshake(mut stream: TcpStream, my_id: &[u8; 16], auth: ClientAuth) -> io::Result<Established> {
    let (esk, epk) = x25519_keypair();
    let mode = match auth {
        ClientAuth::Session(_) => MODE_SESSION,
        ClientAuth::Pair(_) => MODE_PAIR,
        ClientAuth::Guest => MODE_GUEST,
    };
    let ch_bytes = hello_bytes(mode, my_id, &epk);
    stream.write_all(&ch_bytes)?;
    let mut sr = [0u8; 53];
    stream.read_exact(&mut sr)?;
    if &sr[..4] != MAGIC {
        return err("not a Ferry device (bad magic)");
    }
    match sr[4] {
        ST_OK => {}
        ST_UNKNOWN_PEER => return err("the other device does not know us any more - pair again"),
        ST_NOT_PAIRING => return err("the other device is not in pairing mode (or the code expired)"),
        ST_NO_GUESTS => return err("the other device does not accept files from unpaired devices"),
        ST_BUSY => return err("the other device has too many pending transfers - try again later"),
        s => return err(format!("handshake rejected (status {})", s)),
    }
    let mut peer_id = [0u8; 16];
    peer_id.copy_from_slice(&sr[5..21]);
    let mut spk = [0u8; 32];
    spk.copy_from_slice(&sr[21..53]);
    let shared = dh(&esk, &spk)?;
    let mut transcript = Vec::with_capacity(106);
    transcript.extend_from_slice(&ch_bytes);
    transcript.extend_from_slice(&sr);

    match auth {
        ClientAuth::Guest => {
            let mut okm = [0u8; 64];
            hkdf(b"ferry/1 guest", &shared, &[b"ferry/1 guest".as_ref(), &transcript].concat(), &mut okm);
            let (c2s, s2c) = split_keys(&okm);
            Ok(Established {
                ch: Channel { stream, send_key: c2s, recv_key: s2c, send_ctr: 0, recv_ctr: 0 },
                peer_id,
                new_key: None,
                guest: true,
            })
        }
        ClientAuth::Session(key) => {
            let mut okm = [0u8; 64];
            hkdf(key, &shared, &[b"ferry/1 session".as_ref(), &transcript].concat(), &mut okm);
            let (c2s, s2c) = split_keys(&okm);
            Ok(Established {
                ch: Channel { stream, send_key: c2s, recv_key: s2c, send_ctr: 0, recv_ctr: 0 },
                peer_id,
                new_key: None,
                guest: false,
            })
        }
        ClientAuth::Pair(code) => {
            let code = normalize_code(code);
            let mut okm = [0u8; 128];
            hkdf(code.as_bytes(), &shared, &[b"ferry/1 pair".as_ref(), &transcript].concat(), &mut okm);
            let conf: [u8; 32] = okm[..32].try_into().unwrap();
            let lt: [u8; 32] = okm[32..64].try_into().unwrap();
            let (c2s, s2c) = split_keys(&okm[64..128]);
            stream.write_all(&hmac_sha256(&conf, &[b"client"]))?;
            let mut stag = [0u8; 32];
            stream
                .read_exact(&mut stag)
                .or_else(|_| err("pairing failed: wrong code?"))?;
            if !ct_eq(&stag, &hmac_sha256(&conf, &[b"server"])) {
                return err("pairing failed: the other side could not be verified");
            }
            Ok(Established {
                ch: Channel { stream, send_key: c2s, recv_key: s2c, send_ctr: 0, recv_ctr: 0 },
                peer_id,
                new_key: Some(lt),
                guest: false,
            })
        }
    }
}

pub trait ServerCtx {
    fn peer_key(&self, id: &[u8; 16]) -> Option<[u8; 32]>;
    /// Current pairing code (normalized) if a pairing window is open.
    fn pair_code(&self) -> Option<String>;
    fn pair_failed(&self);
    /// ST_OK if an unpaired device at `ip` may open a guest session now.
    fn guest_status(&self, _id: &[u8; 16], _ip: std::net::IpAddr) -> u8 {
        ST_NO_GUESTS
    }
}

pub fn server_handshake(mut stream: TcpStream, my_id: &[u8; 16], ctx: &dyn ServerCtx) -> io::Result<Established> {
    let mut chb = [0u8; 53];
    stream.read_exact(&mut chb)?;
    if &chb[..4] != MAGIC {
        return err("bad magic");
    }
    let mode = chb[4];
    let mut peer_id = [0u8; 16];
    peer_id.copy_from_slice(&chb[5..21]);
    let mut cpk = [0u8; 32];
    cpk.copy_from_slice(&chb[21..53]);

    let reject = |mut s: &TcpStream, st: u8| -> io::Result<Established> {
        let mut b = [0u8; 53];
        b[..4].copy_from_slice(MAGIC);
        b[4] = st;
        let _ = s.write_all(&b);
        err(format!("rejected client (status {})", st))
    };

    let (esk, epk) = x25519_keypair();
    match mode {
        MODE_SESSION => {
            let key = match ctx.peer_key(&peer_id) {
                Some(k) => k,
                None => return reject(&stream, ST_UNKNOWN_PEER),
            };
            let sr = hello_bytes(ST_OK, my_id, &epk);
            stream.write_all(&sr)?;
            let shared = dh(&esk, &cpk)?;
            let transcript = [&chb[..], &sr[..]].concat();
            let mut okm = [0u8; 64];
            hkdf(&key, &shared, &[b"ferry/1 session".as_ref(), &transcript].concat(), &mut okm);
            let (c2s, s2c) = split_keys(&okm);
            Ok(Established {
                ch: Channel { stream, send_key: s2c, recv_key: c2s, send_ctr: 0, recv_ctr: 0 },
                peer_id,
                new_key: None,
                guest: false,
            })
        }
        MODE_PAIR => {
            let code = match ctx.pair_code() {
                Some(c) => c,
                None => return reject(&stream, ST_NOT_PAIRING),
            };
            let sr = hello_bytes(ST_OK, my_id, &epk);
            stream.write_all(&sr)?;
            let shared = dh(&esk, &cpk)?;
            let transcript = [&chb[..], &sr[..]].concat();
            let mut okm = [0u8; 128];
            hkdf(code.as_bytes(), &shared, &[b"ferry/1 pair".as_ref(), &transcript].concat(), &mut okm);
            let conf: [u8; 32] = okm[..32].try_into().unwrap();
            let lt: [u8; 32] = okm[32..64].try_into().unwrap();
            let (c2s, s2c) = split_keys(&okm[64..128]);
            let mut ctag = [0u8; 32];
            stream.read_exact(&mut ctag)?;
            // The client must prove knowledge of the code first, so a fake client
            // only ever gets one online guess per attempt.
            if !ct_eq(&ctag, &hmac_sha256(&conf, &[b"client"])) {
                ctx.pair_failed();
                return err("pairing attempt with wrong code");
            }
            stream.write_all(&hmac_sha256(&conf, &[b"server"]))?;
            Ok(Established {
                ch: Channel { stream, send_key: s2c, recv_key: c2s, send_ctr: 0, recv_ctr: 0 },
                peer_id,
                new_key: Some(lt),
                guest: false,
            })
        }
        MODE_GUEST => {
            let ip = stream.peer_addr().map(|a| a.ip()).unwrap_or(std::net::IpAddr::from([0, 0, 0, 0]));
            let st = ctx.guest_status(&peer_id, ip);
            if st != ST_OK {
                return reject(&stream, st);
            }
            let sr = hello_bytes(ST_OK, my_id, &epk);
            stream.write_all(&sr)?;
            let shared = dh(&esk, &cpk)?;
            let transcript = [&chb[..], &sr[..]].concat();
            let mut okm = [0u8; 64];
            hkdf(b"ferry/1 guest", &shared, &[b"ferry/1 guest".as_ref(), &transcript].concat(), &mut okm);
            let (c2s, s2c) = split_keys(&okm);
            Ok(Established {
                ch: Channel { stream, send_key: s2c, recv_key: c2s, send_ctr: 0, recv_ctr: 0 },
                peer_id,
                new_key: None,
                guest: true,
            })
        }
        _ => reject(&stream, 255),
    }
}

// ---------------------------------------------------------------- UDP discovery

pub const DISC_QUERY: &[u8; 4] = b"FRYQ";
pub const DISC_ANSWER: &[u8; 4] = b"FRYA";

pub fn disc_packet(magic: &[u8; 4], id: &[u8; 16], port: u16) -> [u8; 22] {
    let mut b = [0u8; 22];
    b[..4].copy_from_slice(magic);
    b[4..20].copy_from_slice(id);
    b[20..22].copy_from_slice(&port.to_be_bytes());
    b
}

pub fn parse_disc(b: &[u8]) -> Option<([u8; 4], [u8; 16], u16)> {
    if b.len() < 22 {
        return None;
    }
    let mut m = [0u8; 4];
    m.copy_from_slice(&b[..4]);
    let mut id = [0u8; 16];
    id.copy_from_slice(&b[4..20]);
    Some((m, id, u16::from_be_bytes([b[20], b[21]])))
}

/// v0.2 presence: "FRYP" = who is there? (also announces the sender), "FRYH" = here I am.
/// magic | id[16] | tcp port u16 | kind u8 | flags u8 (bit0: accepts unpaired) | name len u8 | name
pub const PRES_QUERY: &[u8; 4] = b"FRYP";
pub const PRES_HERE: &[u8; 4] = b"FRYH";
pub const PRES_GUESTS: u8 = 1;

pub struct Presence {
    pub magic: [u8; 4],
    pub id: [u8; 16],
    pub port: u16,
    pub kind: u8,
    pub flags: u8,
    pub name: String,
}

pub fn presence_packet(magic: &[u8; 4], id: &[u8; 16], port: u16, kind: u8, flags: u8, name: &str) -> Vec<u8> {
    let mut n = name.as_bytes().to_vec();
    while n.len() > 64 || std::str::from_utf8(&n).is_err() {
        n.pop();
    }
    let mut b = Vec::with_capacity(25 + n.len());
    b.extend_from_slice(magic);
    b.extend_from_slice(id);
    b.extend_from_slice(&port.to_be_bytes());
    b.push(kind);
    b.push(flags);
    b.push(n.len() as u8);
    b.extend_from_slice(&n);
    b
}

pub fn parse_presence(b: &[u8]) -> Option<Presence> {
    if b.len() < 25 || (&b[..4] != PRES_QUERY && &b[..4] != PRES_HERE) {
        return None;
    }
    let nl = b[24] as usize;
    if b.len() < 25 + nl {
        return None;
    }
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&b[..4]);
    let mut id = [0u8; 16];
    id.copy_from_slice(&b[4..20]);
    Some(Presence {
        magic,
        id,
        port: u16::from_be_bytes([b[20], b[21]]),
        kind: b[22],
        flags: b[23],
        name: String::from_utf8_lossy(&b[25..25 + nl]).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::net::TcpListener;

    struct Ctx {
        key: [u8; 32],
        code: Option<String>,
        fails: Cell<u32>,
    }
    impl ServerCtx for Ctx {
        fn peer_key(&self, _: &[u8; 16]) -> Option<[u8; 32]> {
            Some(self.key)
        }
        fn pair_code(&self) -> Option<String> {
            self.code.clone()
        }
        fn pair_failed(&self) {
            self.fails.set(self.fails.get() + 1);
        }
    }

    struct GuestCtx;
    impl ServerCtx for GuestCtx {
        fn peer_key(&self, _: &[u8; 16]) -> Option<[u8; 32]> {
            None
        }
        fn pair_code(&self) -> Option<String> {
            None
        }
        fn pair_failed(&self) {}
        fn guest_status(&self, _: &[u8; 16], _: std::net::IpAddr) -> u8 {
            ST_OK
        }
    }

    #[test]
    fn guest_session_and_presence() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let srv = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut e = server_handshake(s, &[2; 16], &GuestCtx).unwrap();
            assert!(e.guest);
            let b = e.ch.expect(T_OFFER).unwrap();
            assert_eq!(Reader::new(&b).u32().unwrap(), 1);
            e.ch.ack(true, "").unwrap();
            // a server that refuses guests
            let (s, _) = l.accept().unwrap();
            assert!(server_handshake(s, &[2; 16], &Ctx { key: [0; 32], code: None, fails: Cell::new(0) }).is_err());
        });
        let s = TcpStream::connect(addr).unwrap();
        let mut e = client_handshake(s, &[1; 16], ClientAuth::Guest).unwrap();
        e.ch.send(Writer::new(T_OFFER).u32(1).u64(5).u8(0)).unwrap();
        e.ch.wait_ack().unwrap();
        let s = TcpStream::connect(addr).unwrap();
        let r = client_handshake(s, &[1; 16], ClientAuth::Guest);
        assert!(r.err().unwrap().to_string().contains("unpaired"));
        srv.join().unwrap();

        let p = presence_packet(PRES_HERE, &[7; 16], 47800, KIND_PHONE, PRES_GUESTS, "Pixel ✓");
        let q = parse_presence(&p).unwrap();
        assert_eq!((q.port, q.kind, q.flags, q.name.as_str()), (47800, KIND_PHONE, PRES_GUESTS, "Pixel ✓"));
    }

    #[test]
    fn pair_then_session() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let code = new_pair_code();
        let code2 = code.clone();
        let srv = std::thread::spawn(move || {
            let ctx = Ctx { key: [0; 32], code: Some(code2), fails: Cell::new(0) };
            let (s, _) = l.accept().unwrap();
            let mut e = server_handshake(s, &[2; 16], &ctx).unwrap();
            let h = parse_hello(&e.ch.expect(T_HELLO).unwrap()).unwrap();
            assert_eq!(h.name, "client");
            e.ch.send(hello_msg("server", 1)).unwrap();
            let key = e.new_key.unwrap();
            // session
            let ctx = Ctx { key, code: None, fails: Cell::new(0) };
            let (s, _) = l.accept().unwrap();
            let mut e = server_handshake(s, &[2; 16], &ctx).unwrap();
            let b = e.ch.expect(T_CLIP).unwrap();
            assert_eq!(Reader::new(&b).str().unwrap(), "hello clipboard");
            e.ch.ack(true, "").unwrap();
            // wrong pair code
            let ctx = Ctx { key, code: Some("AAAAAAAAAA".into()), fails: Cell::new(0) };
            let (s, _) = l.accept().unwrap();
            assert!(server_handshake(s, &[2; 16], &ctx).is_err());
            assert_eq!(ctx.fails.get(), 1);
        });
        let s = TcpStream::connect(addr).unwrap();
        let mut e = client_handshake(s, &[1; 16], ClientAuth::Pair(&format_code(&code).to_lowercase())).unwrap();
        e.ch.send(hello_msg("client", 2)).unwrap();
        let h = parse_hello(&e.ch.expect(T_HELLO).unwrap()).unwrap();
        assert_eq!(h.name, "server");
        assert_eq!(e.peer_id, [2; 16]);
        let key = e.new_key.unwrap();
        let s = TcpStream::connect(addr).unwrap();
        let mut e = client_handshake(s, &[1; 16], ClientAuth::Session(&key)).unwrap();
        e.ch.send(Writer::new(T_CLIP).str("hello clipboard")).unwrap();
        e.ch.wait_ack().unwrap();
        let s = TcpStream::connect(addr).unwrap();
        assert!(client_handshake(s, &[1; 16], ClientAuth::Pair("BBBBBBBBBB")).is_err());
        srv.join().unwrap();
    }
}
