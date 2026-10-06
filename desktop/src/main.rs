use ferry::{daemon, ipc, sys};
use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

const USAGE: &str = "\
Ferry - share files and clipboard with your phone

USAGE:
  ferry daemon                    run the background service (started automatically)
  ferry status                    show this device, addresses and paired devices
  ferry pair                      show a pairing code; type it into the phone app
  ferry pair <phone-ip> <code>    pair using a code shown on the phone
  ferry send [--to NAME] FILE...  send files (default: most recently used device)
  ferry send --pick               choose files in a dialog, then send
  ferry clip [TEXT]               send TEXT, or the current clipboard
  ferry unpair NAME               forget a paired device
  ferry set KEY VALUE             change a setting:
                                    name, download_dir, auto_clipboard (on/off),
                                    notifications (on/off), port
";

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("ferry: {}", msg);
    exit(1)
}

fn client() -> ipc::Client {
    ipc::Client::connect().unwrap_or_else(|e| die(e))
}

fn finish(r: Result<String, String>) {
    match r {
        Ok(m) => println!("{}", m),
        Err(e) => die(e),
    }
}

fn pick_files() -> Vec<PathBuf> {
    let f = sys::pick_files();
    if f.is_empty() {
        exit(0); // cancelled
    }
    f
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match a.as_slice() {
        ["daemon"] => {
            if let Err(e) = daemon::run() {
                die(e)
            }
        }
        ["status"] | ["peers"] => {
            let mut c = client();
            c.send(&["status"]).unwrap();
            while let Ok(Some(f)) = c.recv() {
                match f[0].as_str() {
                    "status" => {
                        println!("This device : {}", f[1]);
                        println!("Addresses   : {} (port {})", f[5].replace(',', ", "), f[2]);
                        println!("Downloads   : {}", f[3]);
                        println!("Auto clip   : {}", if f[4] == "1" { "on" } else { "off" });
                        println!("Paired devices:");
                    }
                    "peer" => println!(
                        "  - {}  (last seen at {})",
                        f[2],
                        if f[3].is_empty() { "unknown" } else { &f[3] }
                    ),
                    "end" => break,
                    _ => {}
                }
            }
        }
        ["pair"] => {
            let mut c = client();
            c.send(&["subscribe"]).unwrap();
            c.send(&["pairshow"]).unwrap();
            c.set_timeout(Some(Duration::from_secs(300)));
            loop {
                match c.recv() {
                    Ok(Some(f)) => match f[0].as_str() {
                        "pair" => {
                            println!("\nOn the phone: Ferry -> Pair -> enter this:\n");
                            println!("    Address : {}", f[3].replace(',', "  or  "));
                            println!("    Code    : {}\n", f[1]);
                            println!("Waiting for the phone (valid for 5 minutes, Ctrl+C to cancel)...");
                        }
                        "paired" => {
                            println!("Paired with {}.", f[1]);
                            break;
                        }
                        "pairfailed" => die(&f[1]),
                        _ => {}
                    },
                    Ok(None) => die("daemon went away"),
                    Err(_) => die("pairing timed out"),
                }
            }
        }
        ["pair", addr, code] => {
            let mut c = client();
            c.send(&["pairconnect", addr, code]).unwrap();
            finish(c.result());
        }
        ["send", rest @ ..] => {
            let mut to = String::new();
            let mut files: Vec<PathBuf> = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "--to" if i + 1 < rest.len() => {
                        to = rest[i + 1].to_string();
                        i += 1;
                    }
                    "--pick" => files.extend(pick_files()),
                    f => files.push(PathBuf::from(f)),
                }
                i += 1;
            }
            if files.is_empty() {
                die("no files given");
            }
            let abs: Vec<String> = files
                .iter()
                .map(|f| {
                    std::fs::canonicalize(f)
                        .unwrap_or_else(|e| die(format!("{}: {}", f.display(), e)))
                        .to_string_lossy()
                        .to_string()
                })
                .collect();
            let mut fields: Vec<&str> = vec!["sendfiles", &to];
            fields.extend(abs.iter().map(|s| s.as_str()));
            let mut c = client();
            c.send(&fields).unwrap();
            finish(c.result());
        }
        ["clip"] => {
            let mut c = client();
            c.send(&["sendclip"]).unwrap();
            finish(c.result());
        }
        ["clip", text @ ..] => {
            let t = text.join(" ");
            let mut c = client();
            c.send(&["sendclip", &t]).unwrap();
            finish(c.result());
        }
        ["unpair", name] => {
            let mut c = client();
            c.send(&["unpair", name]).unwrap();
            finish(c.result());
        }
        ["set", key, val @ ..] if !val.is_empty() => {
            let v = val.join(" ");
            let mut c = client();
            c.send(&["set", key, &v]).unwrap();
            finish(c.result());
        }
        ["-h"] | ["--help"] | ["help"] | [] => print!("{}", USAGE),
        ["--version"] => println!("ferry {}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprint!("{}", USAGE);
            exit(2)
        }
    }
}
