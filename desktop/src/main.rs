use ferry::{daemon, ipc, sys};
use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

const USAGE: &str = "\
Ferry - share files and the clipboard with your phone and other computers

  My devices = paired with a code: files arrive directly, clipboard syncs automatically.
  Nearby     = any other Ferry device on the network: what they send waits in
               Incoming until you accept it (deleted after 24 h otherwise).

USAGE:
  ferry status                    this device, addresses, paired devices
  ferry devices                   my devices + nearby devices on the network
  ferry send [--to NAME] FILE...  send files (default: your main device, usually the phone)
  ferry send --pick [--to NAME]   choose files in a dialog
  ferry text --to NAME TEXT       send a text (nearby devices get it in Incoming)
  ferry clip [TEXT]               send TEXT or the clipboard to all my devices
  ferry incoming                  transfers from nearby devices waiting for you
  ferry accept ID|all             keep them (files -> Downloads, text -> clipboard)
  ferry decline ID|all            delete them
  ferry block ID|NAME             decline and never accept anything from that device
  ferry unblock all
  ferry queue                     sends waiting for an unreachable device
  ferry cancel ID|all
  ferry pair                      show a pairing code (enter it on the phone / other PC)
  ferry pair <ip> <code>          pair using a code shown on the other device
  ferry unpair NAME
  ferry set KEY VALUE             name, download_dir, auto_clipboard on|off, visible on|off,
                                  notifications on|off, incoming_limit_mb, incoming_hours, port
  ferry daemon                    run the background service (started automatically)
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

fn call(fields: &[&str]) {
    let mut c = client();
    c.send(fields).unwrap();
    finish(c.result());
}

/// Sends a command and collects reply lines starting with `tag` until `end`.
fn list(fields: &[&str], tag: &str, end: &str) -> Vec<Vec<String>> {
    let mut c = client();
    c.send(fields).unwrap();
    let mut out = Vec::new();
    while let Ok(Some(f)) = c.recv() {
        if f[0] == end {
            break;
        }
        if f[0] == tag {
            out.push(f);
        }
    }
    out
}

fn ago(secs: u64) -> String {
    match secs {
        0..=89 => "just now".into(),
        90..=5399 => format!("{} min ago", secs / 60),
        _ => format!("{} h ago", secs / 3600),
    }
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
                let g = |i: usize| f.get(i).cloned().unwrap_or_default();
                match f[0].as_str() {
                    "status" => {
                        println!("This device : {}", g(1));
                        println!("Addresses   : {} (port {})", g(5).replace(',', ", "), g(2));
                        println!("Downloads   : {}", g(3));
                        println!("Clipboard   : {}", if g(4) == "1" { "syncs automatically with my devices" } else { "manual only" });
                        println!(
                            "Nearby      : {}",
                            if g(7) == "1" { "visible - others can send to Incoming" } else { "hidden - only my devices can send" }
                        );
                        if g(8) != "0" && !g(8).is_empty() {
                            println!("Incoming    : {} waiting (ferry incoming)", g(8));
                        }
                        if g(9) != "0" && !g(9).is_empty() {
                            println!("Queued      : {} send(s) waiting for a device (ferry queue)", g(9));
                        }
                        println!("My devices:");
                    }
                    "peer" => println!(
                        "  - {} ({}){}",
                        g(2),
                        g(4),
                        if g(3).is_empty() { String::new() } else { format!(", last at {}", g(3)) }
                    ),
                    "end" => break,
                    _ => {}
                }
            }
        }
        ["devices", "--tsv"] => {
            // id, name, kind, paired|nearby, online - for scripts (file manager integration)
            for d in list(&["devices"], "dev", "devend") {
                println!("{}\t{}\t{}\t{}\t{}", d[1], d[2], d[3], d[4], d[5]);
            }
        }
        ["devices"] => {
            let devs = list(&["devices"], "dev", "devend");
            let (mine, near): (Vec<_>, Vec<_>) = devs.into_iter().partition(|d| d[4] == "paired");
            println!("My devices:");
            if mine.is_empty() {
                println!("  (none yet - ferry pair)");
            }
            for (i, d) in mine.iter().enumerate() {
                println!(
                    "  {} {:<24} {:<6} {}",
                    if i == 0 { "★" } else { "-" },
                    d[2],
                    d[3],
                    if d[5] == "1" { "online" } else { "" }
                );
            }
            println!("Nearby (not paired - what you send waits there for acceptance):");
            if near.is_empty() {
                println!("  (none found)");
            }
            for d in &near {
                println!("  - {:<24} {:<6} {}", d[2], d[3], d[6]);
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
                            println!("\nOn the phone (or other PC): Ferry -> Pair -> enter this:\n");
                            println!("    Address : {}", f[3].replace(',', "  or  "));
                            println!("    Code    : {}\n", f[1]);
                            println!("Waiting (valid for 5 minutes, Ctrl+C to cancel)...");
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
        ["pair", addr, code] => call(&["pairconnect", addr, code]),
        ["send", rest @ ..] => {
            let mut to = String::new();
            let mut files: Vec<PathBuf> = Vec::new();
            let mut pick = false;
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "--to" if i + 1 < rest.len() => {
                        to = rest[i + 1].to_string();
                        i += 1;
                    }
                    "--pick" => pick = true,
                    f => files.push(PathBuf::from(f)),
                }
                i += 1;
            }
            if pick {
                files.extend(sys::pick_files());
                if files.is_empty() {
                    exit(0); // cancelled
                }
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
            call(&fields);
        }
        ["text", "--to", to, text @ ..] if !text.is_empty() => call(&["sendtext", to, &text.join(" ")]),
        ["clip"] => call(&["sendclip"]),
        ["clip", text @ ..] => call(&["sendclip", &text.join(" ")]),
        ["incoming"] => {
            let items = list(&["incoming"], "in", "inend");
            if items.is_empty() {
                println!("Nothing waiting.");
            }
            for f in items {
                let age: u64 = f[7].parse().unwrap_or(0);
                println!("  {}  {} sent {}  ({})", f[1], f[2], f[3], ago(age));
            }
        }
        ["accept", id] => call(&["accept", id]),
        ["decline", id] => call(&["decline", id]),
        ["block", sel] => call(&["block", sel]),
        ["unblock", "all"] => call(&["unblock"]),
        ["queue"] => {
            let jobs = list(&["queue"], "job", "jobend");
            if jobs.is_empty() {
                println!("Nothing queued.");
            }
            for j in jobs {
                println!("  {}  {} -> {}  ({} attempts)", j[1], j[3], j[2], j[4]);
            }
        }
        ["cancel", id] => call(&["cancel", id]),
        ["unpair", name] => call(&["unpair", name]),
        ["set", key, val @ ..] if !val.is_empty() => call(&["set", key, &val.join(" ")]),
        ["-h"] | ["--help"] | ["help"] | [] => print!("{}", USAGE),
        ["--version"] => println!("ferry {}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprint!("{}", USAGE);
            exit(2)
        }
    }
}
