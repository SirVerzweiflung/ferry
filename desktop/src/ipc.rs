//! Local control socket: one command per line, fields separated by TAB,
//! with `\\`, `\t`, `\n`, `\r` escaped. Used by the CLI and the GNOME extension.

use std::io::{self, BufRead, BufReader, Write};
use std::time::Duration;

// ---------------------------------------------------------------- transport
// Linux: a Unix socket in $XDG_RUNTIME_DIR (only the user can open it).
// Windows: TCP on 127.0.0.1 plus a random token stored in the user's profile,
// which every client has to send first.

#[cfg(not(win))]
mod transport {
    use crate::store::socket_path;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};

    pub type Stream = UnixStream;
    pub struct Listener(UnixListener);

    impl Listener {
        pub fn bind() -> io::Result<Listener> {
            let p = socket_path();
            if UnixStream::connect(&p).is_ok() {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, "ferry daemon is already running"));
            }
            let _ = std::fs::remove_file(&p);
            let l = UnixListener::bind(&p)?;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
            Ok(Listener(l))
        }
        pub fn accept(&self) -> io::Result<Stream> {
            self.0.accept().map(|(s, _)| s)
        }
    }

    pub fn connect() -> io::Result<Stream> {
        UnixStream::connect(socket_path())
    }
}

#[cfg(win)]
mod transport {
    use crate::crypto::{ct_eq, random_array, to_hex};
    use crate::store::{config_dir, write_private, Config};
    use std::io::{self, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    pub type Stream = TcpStream;
    pub struct Listener {
        l: TcpListener,
        token: String,
    }

    fn token_file() -> std::path::PathBuf {
        config_dir().join("ipc-token")
    }

    fn read_line_raw(s: &mut TcpStream) -> io::Result<String> {
        let mut out = Vec::new();
        let mut b = [0u8; 1];
        while out.len() < 256 {
            if s.read(&mut b)? == 0 {
                break;
            }
            if b[0] == b'\n' {
                break;
            }
            out.push(b[0]);
        }
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    impl Listener {
        pub fn bind() -> io::Result<Listener> {
            let port = Config::load().port.wrapping_add(1);
            let l = match TcpListener::bind(("127.0.0.1", port)) {
                Ok(l) => l,
                Err(e) if e.kind() == io::ErrorKind::AddrInUse && ferry_answers() => {
                    return Err(io::Error::new(io::ErrorKind::AddrInUse, "ferry is already running"));
                }
                Err(e) => {
                    return Err(io::Error::new(
                        e.kind(),
                        format!(
                            "cannot open the local control port 127.0.0.1:{} ({}). Another program may use it - \
                             change `port` in {} (Ferry uses that port and the next one).",
                            port,
                            e,
                            config_dir().join("config").display()
                        ),
                    ))
                }
            };
            let token = to_hex(&random_array::<16>());
            write_private(&token_file(), &format!("{} {}", port, token))?;
            Ok(Listener { l, token })
        }
        pub fn accept(&self) -> io::Result<Stream> {
            let (mut s, _) = self.l.accept()?;
            s.set_read_timeout(Some(Duration::from_secs(5)))?;
            let line = read_line_raw(&mut s)?;
            let given = line.strip_prefix("auth\t").unwrap_or("");
            if !ct_eq(given.as_bytes(), self.token.as_bytes()) {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "bad ipc token"));
            }
            s.set_read_timeout(None)?;
            Ok(s)
        }
    }

    /// True if a running Ferry instance answers on the control port.
    fn ferry_answers() -> bool {
        let Ok(mut s) = connect() else { return false };
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        if s.write_all(b"status\n").is_err() {
            return false;
        }
        let mut b = [0u8; 7];
        s.read_exact(&mut b).is_ok() && &b[..6] == b"status"
    }

    pub fn connect() -> io::Result<Stream> {
        let t = std::fs::read_to_string(token_file())?;
        let mut it = t.split_whitespace();
        let port: u16 = it.next().and_then(|p| p.parse().ok()).unwrap_or(47801);
        let token = it.next().unwrap_or("");
        let mut s = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2))?;
        s.write_all(format!("auth\t{}\n", token).as_bytes())?;
        Ok(s)
    }
}

pub use transport::{connect, Listener, Stream};

pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\t' => o.push_str("\\t"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            c => o.push(c),
        }
    }
    o
}

pub fn unescape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('t') => o.push('\t'),
                Some('n') => o.push('\n'),
                Some('r') => o.push('\r'),
                Some(x) => o.push(x),
                None => {}
            }
        } else {
            o.push(c);
        }
    }
    o
}

pub fn line(fields: &[&str]) -> String {
    let mut s = fields.iter().map(|f| escape(f)).collect::<Vec<_>>().join("\t");
    s.push('\n');
    s
}

pub fn split(l: &str) -> Vec<String> {
    l.trim_end_matches(['\n', '\r']).split('\t').map(unescape).collect()
}

/// CLI-side connection to the daemon.
pub struct Client {
    w: Stream,
    r: BufReader<Stream>,
}

impl Client {
    pub fn connect() -> io::Result<Client> {
        let hint = if cfg!(win) { "start Ferry from the Start menu" } else { "systemctl --user start ferry" };
        let s = connect().map_err(|e| {
            io::Error::new(e.kind(), format!("cannot reach the Ferry daemon ({}). Start it with: {}", e, hint))
        })?;
        Ok(Client { r: BufReader::new(s.try_clone()?), w: s })
    }

    pub fn send(&mut self, fields: &[&str]) -> io::Result<()> {
        self.w.write_all(line(fields).as_bytes())
    }

    pub fn set_timeout(&self, d: Option<Duration>) {
        let _ = self.w.set_read_timeout(d);
    }

    /// Next reply line; None on EOF.
    pub fn recv(&mut self) -> io::Result<Option<Vec<String>>> {
        let mut l = String::new();
        if self.r.read_line(&mut l)? == 0 {
            return Ok(None);
        }
        Ok(Some(split(&l)))
    }

    /// Waits for an `ok` / `err` line and converts it into a Result.
    pub fn result(&mut self) -> Result<String, String> {
        loop {
            match self.recv() {
                Ok(Some(f)) => match f[0].as_str() {
                    "ok" => return Ok(f.get(1).cloned().unwrap_or_default()),
                    "err" => return Err(f.get(1).cloned().unwrap_or_default()),
                    _ => continue,
                },
                Ok(None) => return Err("daemon closed the connection".into()),
                Err(e) => return Err(e.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let s = "a\tb\nc\\d\re";
        assert_eq!(unescape(&escape(s)), s);
        assert_eq!(split(&line(&["x", s, ""])), vec!["x".to_string(), s.to_string(), "".to_string()]);
    }
}
