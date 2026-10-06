//! Windows glue: tray icon + menu, toast notifications, clipboard listener,
//! file picker and pairing dialog. Plain Win32 through a few FFI declarations
//! (no crates). Idle cost: one thread blocked in GetMessageW - no polling.
//! Clipboard changes arrive as WM_CLIPBOARDUPDATE events.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use crate::daemon::State;
use std::ffi::c_void;
use std::net::{ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

type HANDLE = isize;
type HWND = isize;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;
type WNDPROC = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

#[repr(C)]
#[derive(Default)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Default)]
struct MSG {
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    time: u32,
    pt: POINT,
    private: u32,
}

#[repr(C)]
struct WNDCLASSEXW {
    cb_size: u32,
    style: u32,
    wnd_proc: WNDPROC,
    cls_extra: i32,
    wnd_extra: i32,
    instance: HANDLE,
    icon: HANDLE,
    cursor: HANDLE,
    background: HANDLE,
    menu_name: *const u16,
    class_name: *const u16,
    icon_sm: HANDLE,
}

#[repr(C)]
struct GUID {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

#[repr(C)]
struct NOTIFYICONDATAW {
    cb_size: u32,
    hwnd: HWND,
    uid: u32,
    flags: u32,
    callback_message: u32,
    icon: HANDLE,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    version: u32, // union with uTimeout
    info_title: [u16; 64],
    info_flags: u32,
    guid: GUID,
    balloon_icon: HANDLE,
}

#[repr(C)]
struct ICONINFO {
    f_icon: i32,
    x_hotspot: u32,
    y_hotspot: u32,
    mask: HANDLE,
    color: HANDLE,
}

#[repr(C)]
struct OPENFILENAMEW {
    struct_size: u32,
    owner: HWND,
    instance: HANDLE,
    filter: *const u16,
    custom_filter: *mut u16,
    max_cust_filter: u32,
    filter_index: u32,
    file: *mut u16,
    max_file: u32,
    file_title: *mut u16,
    max_file_title: u32,
    initial_dir: *const u16,
    title: *const u16,
    flags: u32,
    file_offset: u16,
    file_extension: u16,
    def_ext: *const u16,
    cust_data: LPARAM,
    hook: *const c_void,
    template_name: *const u16,
    reserved_ptr: *mut c_void,
    reserved: u32,
    flags_ex: u32,
}

// Only kernel32 is linked at build time (always available, also with the GNU toolchain
// that needs no Visual Studio). Everything else is resolved at runtime via GetProcAddress.
#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> HANDLE;
    fn GetProcAddress(module: HANDLE, name: *const u8) -> *const c_void;
    fn GetModuleHandleW(name: *const u16) -> HANDLE;
    fn GlobalAlloc(flags: u32, bytes: usize) -> HANDLE;
    fn GlobalLock(h: HANDLE) -> *mut c_void;
    fn GlobalUnlock(h: HANDLE) -> i32;
    fn GlobalFree(h: HANDLE) -> HANDLE;
    fn Sleep(ms: u32);
}

fn resolve(lib: &str, name: &str) -> usize {
    let l = wide(lib);
    let n: Vec<u8> = name.bytes().chain(std::iter::once(0)).collect();
    unsafe {
        let m = LoadLibraryW(l.as_ptr());
        let p = if m != 0 { GetProcAddress(m, n.as_ptr()) } else { null() };
        if p.is_null() {
            eprintln!("ferry: {}!{} missing", lib, name);
            std::process::abort();
        }
        p as usize
    }
}

macro_rules! dynfn {
    ($lib:literal, fn $name:ident($($a:ident: $t:ty),*) -> $r:ty) => {
        unsafe fn $name($($a: $t),*) -> $r {
            static P: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let mut p = P.load(Ordering::Relaxed);
            if p == 0 {
                p = resolve($lib, stringify!($name));
                P.store(p, Ordering::Relaxed);
            }
            let f: unsafe extern "system" fn($($t),*) -> $r = std::mem::transmute(p);
            f($($a),*)
        }
    };
}

dynfn!("user32.dll", fn RegisterClassExW(wc: *const WNDCLASSEXW) -> u16);
dynfn!("user32.dll", fn CreateWindowExW(ex: u32, class: *const u16, name: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: HWND, menu: HANDLE, inst: HANDLE, param: *mut c_void) -> HWND);
dynfn!("user32.dll", fn DefWindowProcW(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT);
dynfn!("user32.dll", fn GetMessageW(m: *mut MSG, h: HWND, min: u32, max: u32) -> i32);
dynfn!("user32.dll", fn TranslateMessage(m: *const MSG) -> i32);
dynfn!("user32.dll", fn DispatchMessageW(m: *const MSG) -> LRESULT);
dynfn!("user32.dll", fn PostMessageW(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> i32);
dynfn!("user32.dll", fn PostQuitMessage(code: i32) -> ());
dynfn!("user32.dll", fn CreatePopupMenu() -> HANDLE);
dynfn!("user32.dll", fn AppendMenuW(menu: HANDLE, flags: u32, id: usize, text: *const u16) -> i32);
dynfn!("user32.dll", fn TrackPopupMenu(menu: HANDLE, flags: u32, x: i32, y: i32, r: i32, h: HWND, rect: *const c_void) -> i32);
dynfn!("user32.dll", fn DestroyMenu(menu: HANDLE) -> i32);
dynfn!("user32.dll", fn SetForegroundWindow(h: HWND) -> i32);
dynfn!("user32.dll", fn GetCursorPos(p: *mut POINT) -> i32);
dynfn!("user32.dll", fn MessageBoxW(h: HWND, text: *const u16, caption: *const u16, t: u32) -> i32);
dynfn!("user32.dll", fn FindWindowW(class: *const u16, name: *const u16) -> HWND);
dynfn!("user32.dll", fn RegisterWindowMessageW(s: *const u16) -> u32);
dynfn!("user32.dll", fn OpenClipboard(h: HWND) -> i32);
dynfn!("user32.dll", fn CloseClipboard() -> i32);
dynfn!("user32.dll", fn EmptyClipboard() -> i32);
dynfn!("user32.dll", fn GetClipboardData(fmt: u32) -> HANDLE);
dynfn!("user32.dll", fn SetClipboardData(fmt: u32, h: HANDLE) -> HANDLE);
dynfn!("user32.dll", fn IsClipboardFormatAvailable(fmt: u32) -> i32);
dynfn!("user32.dll", fn RegisterClipboardFormatW(name: *const u16) -> u32);
dynfn!("user32.dll", fn AddClipboardFormatListener(h: HWND) -> i32);
dynfn!("user32.dll", fn CreateIconIndirect(ii: *const ICONINFO) -> HANDLE);
dynfn!("user32.dll", fn DestroyWindow(h: HWND) -> i32);
dynfn!("shell32.dll", fn Shell_NotifyIconW(msg: u32, data: *const NOTIFYICONDATAW) -> i32);
dynfn!("shell32.dll", fn ShellExecuteW(h: HWND, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> HANDLE);
dynfn!("comdlg32.dll", fn GetOpenFileNameW(ofn: *mut OPENFILENAMEW) -> i32);
dynfn!("gdi32.dll", fn CreateBitmap(w: i32, h: i32, planes: u32, bpp: u32, bits: *const c_void) -> HANDLE);
dynfn!("ole32.dll", fn CoInitializeEx(reserved: *const c_void, coinit: u32) -> i32);

const WM_DESTROY: u32 = 0x0002;
const WM_CLOSE: u32 = 0x0010;
const WM_CLIPBOARDUPDATE: u32 = 0x031D;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_APP_TRAY: u32 = 0x8000 + 1;
const WM_APP_EVENT: u32 = 0x8000 + 2;
const NIN_BALLOONUSERCLICK: u32 = 0x0400 + 5;
const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIF_MESSAGE: u32 = 0x1;
const NIF_ICON: u32 = 0x2;
const NIF_TIP: u32 = 0x4;
const NIF_INFO: u32 = 0x10;
const NIIF_INFO: u32 = 0x1;
const MF_STRING: u32 = 0x0;
const MF_GRAYED: u32 = 0x1;
const MF_CHECKED: u32 = 0x8;
const MF_SEPARATOR: u32 = 0x800;
const TPM_RIGHTBUTTON: u32 = 0x2;
const TPM_RETURNCMD: u32 = 0x100;
const MB_ICONERROR: u32 = 0x10;
const MB_ICONINFORMATION: u32 = 0x40;
const MB_SETFOREGROUND: u32 = 0x10000;
const MB_TOPMOST: u32 = 0x40000;
const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x2;
const SW_SHOWNORMAL: i32 = 1;
const OFN_ALLOWMULTISELECT: u32 = 0x200;
const OFN_PATHMUSTEXIST: u32 = 0x800;
const OFN_FILEMUSTEXIST: u32 = 0x1000;
const OFN_EXPLORER: u32 = 0x80000;
const OFN_NOCHANGEDIR: u32 = 0x8;
const COINIT_APARTMENTTHREADED: u32 = 0x2;

const PAIR_TITLE: &str = "Ferry - pair a phone";
const ID_SEND_CLIP: usize = 2;
const ID_PAIR: usize = 4;
const ID_AUTO: usize = 5;
const ID_OPEN: usize = 6;
const ID_QUIT: usize = 7;
const ID_VISIBLE: usize = 8;
const ID_DEVICE_BASE: usize = 100; // + index into the device list
const ID_INCOMING_BASE: usize = 1000; // + 3 * index + {0 accept, 1 decline, 2 block}
const MF_POPUP: u32 = 0x10;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn copy_w(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for c in s.encode_utf16() {
        if n + 1 >= dst.len() {
            break;
        }
        dst[n] = c;
        n += 1;
    }
    dst[n] = 0;
}

enum Ev {
    Incoming(String, String),
    SetClip(String),
    Notify(String, String, Option<PathBuf>),
    Paired(String),
    PairFailed(String),
}

struct Gui {
    st: Arc<State>,
    hwnd: AtomicIsize,
    icon: AtomicIsize,
    taskbar_created: AtomicU32,
    queue: Mutex<Vec<Ev>>,
    last_open: Mutex<Option<PathBuf>>,
    /// The last notification was about Incoming: clicking it opens the tray menu.
    last_is_incoming: std::sync::atomic::AtomicBool,
    /// Device list / incoming list behind the currently open menu.
    menu_devices: Mutex<Vec<crate::daemon::DevInfo>>,
    menu_incoming: Mutex<Vec<String>>,
}

static GUI: OnceLock<Gui> = OnceLock::new();

fn post(ev: Ev) -> bool {
    let Some(g) = GUI.get() else { return false };
    g.queue.lock().unwrap().push(ev);
    let h = g.hwnd.load(Ordering::SeqCst);
    if h != 0 {
        unsafe { PostMessageW(h, WM_APP_EVENT, 0, 0) };
    }
    true
}

// ---------------------------------------------------------------- API used by the daemon

pub fn notify(title: &str, body: &str) {
    if !post(Ev::Notify(title.into(), body.into(), None)) {
        eprintln!("[notify] {}: {}", title, body);
    }
}

/// Transfer from an unpaired device: a normal notification (no dialog). Clicking it opens
/// the tray menu, where Incoming has Accept / Decline / Block.
pub fn notify_incoming(title: &str, body: &str, _on_choice: Box<dyn FnOnce(&str) + Send>) {
    post(Ev::Incoming(title.into(), format!("{} Click to choose.", body)));
}

pub fn notify_file(title: &str, body: &str, open: &Path) {
    post(Ev::Notify(title.into(), body.into(), Some(open.to_path_buf())));
}

pub fn on_event(f: &[&str]) {
    match f.first().copied() {
        Some("paired") => {
            post(Ev::Paired(f.get(1).unwrap_or(&"").to_string()));
        }
        Some("pairfailed") => {
            post(Ev::PairFailed(f.get(1).unwrap_or(&"").to_string()));
        }
        _ => {}
    }
}

pub fn clipboard_set(text: &str) -> bool {
    post(Ev::SetClip(text.to_string()))
}

fn clipboard_open(owner: HWND) -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(owner) } != 0 {
            return true;
        }
        unsafe { Sleep(20) }; // another app holds it for a moment
    }
    false
}

pub fn clipboard_get() -> Option<String> {
    if !clipboard_open(0) {
        return None;
    }
    let mut out = None;
    unsafe {
        let h = GetClipboardData(CF_UNICODETEXT);
        if h != 0 {
            let p = GlobalLock(h) as *const u16;
            if !p.is_null() {
                let mut n = 0;
                while *p.add(n) != 0 {
                    n += 1;
                }
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
                out = Some(s.replace("\r\n", "\n"));
                GlobalUnlock(h);
            }
        }
        CloseClipboard();
    }
    out
}

/// Password managers (KeePass, 1Password, Bitwarden...) mark secrets with these formats.
fn clipboard_is_secret() -> bool {
    ["ExcludeClipboardContentFromMonitorProcessing", "Clipboard Viewer Ignore"].iter().any(|n| {
        let w = wide(n);
        unsafe {
            let f = RegisterClipboardFormatW(w.as_ptr());
            f != 0 && IsClipboardFormatAvailable(f) != 0
        }
    })
}

fn clipboard_write(hwnd: HWND, text: &str) {
    let w: Vec<u16> = text.replace("\r\n", "\n").replace('\n', "\r\n").encode_utf16().chain(std::iter::once(0)).collect();
    if !clipboard_open(hwnd) {
        return;
    }
    unsafe {
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2);
        if h != 0 {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                GlobalUnlock(h);
                if SetClipboardData(CF_UNICODETEXT, h) == 0 {
                    GlobalFree(h);
                }
            } else {
                GlobalFree(h);
            }
        }
        CloseClipboard();
    }
}

pub fn local_addrs() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.connect("192.0.2.1:9").is_ok() {
            if let Ok(a) = s.local_addr() {
                v.push(a.ip().to_string());
            }
        }
    }
    // On Windows, resolving the own computer name lists the addresses of all adapters.
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        if let Ok(it) = (name.as_str(), 0).to_socket_addrs() {
            for a in it {
                if let std::net::IpAddr::V4(ip) = a.ip() {
                    let s = ip.to_string();
                    if !ip.is_loopback() && !ip.is_link_local() && !v.contains(&s) {
                        v.push(s);
                    }
                }
            }
        }
    }
    v
}

pub fn pick_files() -> Vec<PathBuf> {
    unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED) };
    let mut buf = vec![0u16; 65536];
    let filter: Vec<u16> = "All files\0*.*\0\0".encode_utf16().collect();
    let title = wide("Send to phone");
    let mut ofn = OPENFILENAMEW {
        struct_size: std::mem::size_of::<OPENFILENAMEW>() as u32,
        owner: 0,
        instance: 0,
        filter: filter.as_ptr(),
        custom_filter: null_mut(),
        max_cust_filter: 0,
        filter_index: 1,
        file: buf.as_mut_ptr(),
        max_file: buf.len() as u32,
        file_title: null_mut(),
        max_file_title: 0,
        initial_dir: null(),
        title: title.as_ptr(),
        flags: OFN_ALLOWMULTISELECT | OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        file_offset: 0,
        file_extension: 0,
        def_ext: null(),
        cust_data: 0,
        hook: null(),
        template_name: null(),
        reserved_ptr: null_mut(),
        reserved: 0,
        flags_ex: 0,
    };
    if unsafe { GetOpenFileNameW(&mut ofn) } == 0 {
        return Vec::new();
    }
    // Single file: "C:\dir\file\0\0". Several: "C:\dir\0a.txt\0b.txt\0\0".
    let parts: Vec<String> = buf
        .split(|c| *c == 0)
        .take_while(|p| !p.is_empty())
        .map(String::from_utf16_lossy)
        .collect();
    match parts.len() {
        0 => Vec::new(),
        1 => vec![PathBuf::from(&parts[0])],
        _ => parts[1..].iter().map(|f| Path::new(&parts[0]).join(f)).collect(),
    }
}

/// Background apps have no console: send stderr (all `eprintln!` output) to
/// %APPDATA%\Ferry\ferry.log so problems can be diagnosed.
pub fn init_logging() {
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        #[link(name = "kernel32")]
        extern "system" {
            fn SetStdHandle(which: u32, h: *mut c_void) -> i32;
        }
        const STD_ERROR_HANDLE: u32 = -12i32 as u32;
        let dir = crate::store::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("ferry.log");
        if std::fs::metadata(&path).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
            let _ = std::fs::rename(&path, dir.join("ferry.old.log"));
        }
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            unsafe { SetStdHandle(STD_ERROR_HANDLE, f.into_raw_handle() as *mut c_void) };
        }
    }
}

/// Small popup menu at the mouse pointer to pick a device (Explorer "Send to" chooser).
/// `devices`: (id, label). Returns the chosen id.
pub fn choose_device(title: &str, devices: &[(String, String)]) -> Option<String> {
    unsafe {
        let class = wide("STATIC");
        let empty = wide("");
        let h = CreateWindowExW(0, class.as_ptr(), empty.as_ptr(), 0, 0, 0, 0, 0, 0, 0, GetModuleHandleW(null()), null_mut());
        let m = CreatePopupMenu();
        let t = wide(title);
        AppendMenuW(m, MF_STRING | MF_GRAYED, 1, t.as_ptr());
        AppendMenuW(m, MF_SEPARATOR, 0, null());
        for (i, (_, label)) in devices.iter().enumerate() {
            let w = wide(label);
            AppendMenuW(m, if label.starts_with("--") { MF_STRING | MF_GRAYED } else { MF_STRING }, 100 + i, w.as_ptr());
        }
        let mut pt = POINT::default();
        GetCursorPos(&mut pt);
        SetForegroundWindow(h);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, h, null()) as usize;
        DestroyMenu(m);
        if h != 0 {
            DestroyWindow(h);
        }
        cmd.checked_sub(100).and_then(|i| devices.get(i)).map(|d| d.0.clone()).filter(|id| !id.is_empty())
    }
}

pub fn error_box(msg: &str) {
    let (t, c) = (wide(msg), wide("Ferry"));
    unsafe { MessageBoxW(0, t.as_ptr(), c.as_ptr(), MB_ICONERROR | MB_SETFOREGROUND) };
}

fn shell_open(p: &Path) {
    let (op, explorer) = (wide("open"), wide("explorer.exe"));
    unsafe {
        if p.is_file() {
            let args = wide(&format!("/select,\"{}\"", p.display()));
            ShellExecuteW(0, op.as_ptr(), explorer.as_ptr(), args.as_ptr(), null(), SW_SHOWNORMAL);
        } else {
            let f = wide(&p.to_string_lossy());
            ShellExecuteW(0, op.as_ptr(), f.as_ptr(), null(), null(), SW_SHOWNORMAL);
        }
    }
}

// ---------------------------------------------------------------- tray icon

/// Draws the Ferry icon (two arrows on a teal disc) into a 32x32 ARGB icon.
fn make_icon() -> HANDLE {
    const N: usize = 32;
    let top = [(34.0, 40.0), (64.0, 40.0), (64.0, 32.0), (78.0, 44.0), (64.0, 56.0), (64.0, 48.0), (34.0, 48.0)];
    let bot = [(74.0, 60.0), (44.0, 60.0), (44.0, 52.0), (30.0, 64.0), (44.0, 76.0), (44.0, 68.0), (74.0, 68.0)];
    fn inside(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
        let mut c = false;
        let mut j = poly.len() - 1;
        for i in 0..poly.len() {
            let (xi, yi) = poly[i];
            let (xj, yj) = poly[j];
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                c = !c;
            }
            j = i;
        }
        c
    }
    let mut px = vec![0u32; N * N];
    for y in 0..N {
        for x in 0..N {
            let (mut disc, mut arrow) = (0u32, 0u32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = x as f32 + (sx as f32 + 0.5) / 4.0;
                    let fy = y as f32 + (sy as f32 + 0.5) / 4.0;
                    let (dx, dy) = (fx - 16.0, fy - 16.0);
                    if dx * dx + dy * dy <= 15.5 * 15.5 {
                        disc += 1;
                        // map icon pixels onto the 108-unit Android artwork (24..84 visible)
                        let (ax, ay) = (fx * 60.0 / 32.0 + 24.0, fy * 60.0 / 32.0 + 24.0);
                        if inside(&top, ax, ay) || inside(&bot, ax, ay) {
                            arrow += 1;
                        }
                    }
                }
            }
            if disc == 0 {
                continue;
            }
            let a = disc * 255 / 16;
            let t = arrow as f32 / disc as f32;
            let mix = |bg: f32| (bg + (255.0 - bg) * t) as u32;
            let (r, g, b) = (mix(0x14 as f32), mix(0x96 as f32), mix(0x7F as f32));
            px[y * N + x] = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }
    let mask = vec![0u8; N * N / 8];
    unsafe {
        let color = CreateBitmap(N as i32, N as i32, 1, 32, px.as_ptr() as *const c_void);
        let mask_bm = CreateBitmap(N as i32, N as i32, 1, 1, mask.as_ptr() as *const c_void);
        let ii = ICONINFO { f_icon: 1, x_hotspot: 0, y_hotspot: 0, mask: mask_bm, color };
        CreateIconIndirect(&ii)
    }
}

fn nid(g: &Gui) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cb_size: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hwnd: g.hwnd.load(Ordering::SeqCst),
        uid: 1,
        flags: 0,
        callback_message: WM_APP_TRAY,
        icon: g.icon.load(Ordering::SeqCst),
        tip: [0; 128],
        state: 0,
        state_mask: 0,
        info: [0; 256],
        version: 0,
        info_title: [0; 64],
        info_flags: 0,
        guid: GUID { d1: 0, d2: 0, d3: 0, d4: [0; 8] },
        balloon_icon: 0,
    }
}

fn tray_add(g: &Gui) -> bool {
    let mut d = nid(g);
    d.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    copy_w(&mut d.tip, "Ferry - phone sharing");
    unsafe {
        Shell_NotifyIconW(NIM_DELETE, &d); // in case an old icon is still registered
        Shell_NotifyIconW(NIM_ADD, &d) != 0
    }
}

fn toast(g: &Gui, title: &str, body: &str) {
    let mut d = nid(g);
    d.flags = NIF_INFO;
    d.info_flags = NIIF_INFO;
    copy_w(&mut d.info_title, title);
    copy_w(&mut d.info, if body.is_empty() { " " } else { body });
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
}

fn show_menu(g: &Gui) {
    let st = &g.st;
    // short network scan so nearby devices are current (~0.7 s)
    let devices = st.devices(true);
    let incoming = st.incoming();
    let peers = st.peer_names();
    let status = if peers.is_empty() {
        format!("{} - nothing paired yet", st.device_name())
    } else {
        format!("{}  \u{21c4}  {}", st.device_name(), peers.join(", "))
    };
    unsafe {
        let m = CreatePopupMenu();
        let add = |menu: HANDLE, flags: u32, id: usize, text: &str| {
            let w = wide(text);
            AppendMenuW(menu, flags, id, w.as_ptr());
        };
        add(m, MF_STRING | MF_GRAYED, 1, &status);
        AppendMenuW(m, MF_SEPARATOR, 0, null());

        if !incoming.is_empty() {
            add(m, MF_STRING | MF_GRAYED, 1, &format!("Incoming from devices that are not paired ({})", incoming.len()));
            for (i, p) in incoming.iter().enumerate() {
                let sub = CreatePopupMenu();
                let base = ID_INCOMING_BASE + 3 * i;
                add(sub, MF_STRING, base, "Accept");
                add(sub, MF_STRING, base + 1, "Decline");
                add(sub, MF_STRING, base + 2, &format!("Decline and block {}", p.from_name));
                add(m, MF_POPUP, sub as usize, &format!("    {}: {}", p.from_name, p.summary()));
            }
            AppendMenuW(m, MF_SEPARATOR, 0, null());
        }

        add(m, MF_STRING, ID_SEND_CLIP, "Send clipboard to my devices");
        let sub = CreatePopupMenu();
        add(sub, MF_STRING | MF_GRAYED, 1, "My devices");
        let mine: Vec<usize> = (0..devices.len()).filter(|i| devices[*i].paired).collect();
        let near: Vec<usize> = (0..devices.len()).filter(|i| !devices[*i].paired).collect();
        if mine.is_empty() {
            add(sub, MF_STRING | MF_GRAYED, 1, "    none paired yet");
        }
        for (n, i) in mine.iter().enumerate() {
            let d = &devices[*i];
            let label = format!(
                "{}{}{}",
                if n == 0 { "\u{2605} " } else { "" },
                d.name,
                if d.online { "" } else { "  (offline - will be queued)" }
            );
            add(sub, MF_STRING, ID_DEVICE_BASE + i, &label);
        }
        AppendMenuW(sub, MF_SEPARATOR, 0, null());
        add(sub, MF_STRING | MF_GRAYED, 1, "Nearby - they have to accept");
        if near.is_empty() {
            add(sub, MF_STRING | MF_GRAYED, 1, "    none found");
        }
        for i in &near {
            let d = &devices[*i];
            add(sub, MF_STRING, ID_DEVICE_BASE + i, &format!("{}  ({})", d.name, crate::daemon::kind_str(d.kind)));
        }
        add(m, MF_POPUP, sub as usize, "Send files to");
        add(m, MF_STRING, ID_PAIR, "Pair a new device\u{2026}");
        AppendMenuW(m, MF_SEPARATOR, 0, null());
        add(m, MF_STRING | if st.auto_clipboard() { MF_CHECKED } else { 0 }, ID_AUTO, "Sync clipboard with my devices");
        add(m, MF_STRING | if st.visible() { MF_CHECKED } else { 0 }, ID_VISIBLE, "Visible to nearby devices");
        add(m, MF_STRING, ID_OPEN, "Open received files");
        AppendMenuW(m, MF_SEPARATOR, 0, null());
        add(m, MF_STRING, ID_QUIT, "Quit Ferry");

        *g.menu_devices.lock().unwrap() = devices;
        *g.menu_incoming.lock().unwrap() = incoming.iter().map(|p| p.id.clone()).collect();

        let mut pt = POINT::default();
        GetCursorPos(&mut pt);
        let hwnd = g.hwnd.load(Ordering::SeqCst);
        SetForegroundWindow(hwnd); // required so the menu closes when clicking elsewhere
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, null());
        DestroyMenu(m); // also destroys the submenus
        menu_command(g, cmd as usize);
    }
}

fn menu_command(g: &Gui, cmd: usize) {
    let st = g.st.clone();
    match cmd {
        ID_SEND_CLIP => match clipboard_get().filter(|t| !t.is_empty()) {
            Some(text) => {
                std::thread::spawn(move || {
                    if let Ok(m) = st.send_clip_now(Some(text)) {
                        notify("Ferry", &(m[..1].to_uppercase() + &m[1..]));
                    }
                });
            }
            None => notify("Ferry", "The clipboard does not contain text."),
        },
        c if (ID_DEVICE_BASE..ID_INCOMING_BASE).contains(&c) => {
            let dev = g.menu_devices.lock().unwrap().get(c - ID_DEVICE_BASE).cloned();
            if let Some(d) = dev {
                std::thread::spawn(move || {
                    let files = pick_files();
                    if !files.is_empty() {
                        let _ = st.send_to(&crate::crypto::to_hex(&d.id), crate::daemon::Payload::Files(files));
                    }
                });
            }
        }
        c if c >= ID_INCOMING_BASE => {
            let k = c - ID_INCOMING_BASE;
            let id = g.menu_incoming.lock().unwrap().get(k / 3).cloned();
            if let Some(id) = id {
                std::thread::spawn(move || {
                    let r = match k % 3 {
                        0 => st.accept(&id),
                        1 => st.decline(&id),
                        _ => st.block(&id),
                    };
                    if let Err(e) = r {
                        notify("Ferry", &e);
                    }
                });
            }
        }
        ID_VISIBLE => {
            let on = !st.visible();
            let _ = st.set_option("visible", if on { "on" } else { "off" });
        }
        ID_PAIR => {
            let (code, addrs) = st.pair_show();
            let addr = if addrs.is_empty() { "(no network found)".to_string() } else { addrs.join("   or   ") };
            let text = format!(
                "On the phone open Ferry, tap \"Pair with computer\" and enter:\n\n\
                 Address:   {}\n\nCode:        {}\n\nThe code is valid for 5 minutes.\n\
                 This window closes by itself when pairing succeeds.",
                addr, code
            );
            std::thread::spawn(move || {
                let (t, c) = (wide(&text), wide(PAIR_TITLE));
                unsafe {
                    MessageBoxW(0, t.as_ptr(), c.as_ptr(), MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST)
                };
            });
        }
        ID_AUTO => {
            let on = !st.auto_clipboard();
            let _ = st.set_option("auto_clipboard", if on { "on" } else { "off" });
        }
        ID_OPEN => {
            let d = st.download_dir();
            let _ = std::fs::create_dir_all(&d);
            shell_open(&d);
        }
        ID_QUIT => unsafe {
            let d = nid(g);
            Shell_NotifyIconW(NIM_DELETE, &d);
            PostQuitMessage(0);
        },
        _ => {}
    }
}

fn close_pair_dialog() {
    let t = wide(PAIR_TITLE);
    let h = unsafe { FindWindowW(null(), t.as_ptr()) };
    if h != 0 {
        unsafe { PostMessageW(h, WM_CLOSE, 0, 0) };
    }
}

fn drain_events(g: &Gui) {
    let evs: Vec<Ev> = std::mem::take(&mut *g.queue.lock().unwrap());
    let hwnd = g.hwnd.load(Ordering::SeqCst);
    for ev in evs {
        match ev {
            Ev::SetClip(t) => clipboard_write(hwnd, &t),
            Ev::Incoming(title, body) => {
                *g.last_open.lock().unwrap() = None;
                g.last_is_incoming.store(true, Ordering::SeqCst);
                toast(g, &title, &body);
            }
            Ev::Notify(title, body, open) => {
                g.last_is_incoming.store(false, Ordering::SeqCst);
                *g.last_open.lock().unwrap() = open;
                toast(g, &title, &body);
            }
            Ev::Paired(name) => {
                close_pair_dialog();
                *g.last_open.lock().unwrap() = None;
                toast(g, "Paired", &format!("Ferry is now connected to {}.", name));
            }
            Ev::PairFailed(msg) => {
                close_pair_dialog();
                toast(g, "Pairing cancelled", &msg);
            }
        }
    }
}

unsafe extern "system" fn wnd_proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let Some(g) = GUI.get() else { return DefWindowProcW(h, msg, w, l) };
    match msg {
        WM_APP_TRAY => {
            match (l as u32) & 0xffff {
                WM_LBUTTONUP | WM_RBUTTONUP => show_menu(g),
                NIN_BALLOONUSERCLICK if g.last_is_incoming.load(Ordering::SeqCst) => show_menu(g),
                NIN_BALLOONUSERCLICK => {
                    let p = g.last_open.lock().unwrap().clone();
                    if let Some(p) = p {
                        shell_open(&p);
                    }
                }
                _ => {}
            }
            0
        }
        WM_APP_EVENT => {
            drain_events(g);
            0
        }
        WM_CLIPBOARDUPDATE => {
            if !clipboard_is_secret() {
                if let Some(t) = clipboard_get() {
                    g.st.clipboard_changed(t);
                }
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        m if m != 0 && m == g.taskbar_created.load(Ordering::SeqCst) => {
            tray_add(g); // Explorer restarted: put the icon back
            0
        }
        _ => DefWindowProcW(h, msg, w, l),
    }
}

/// Runs the tray icon and clipboard listener on the daemon's main thread until "Quit".
pub fn main_loop(st: Arc<State>) -> std::io::Result<()> {
    let g = GUI.get_or_init(|| Gui {
        st,
        hwnd: AtomicIsize::new(0),
        icon: AtomicIsize::new(0),
        taskbar_created: AtomicU32::new(0),
        queue: Mutex::new(Vec::new()),
        last_open: Mutex::new(None),
        last_is_incoming: std::sync::atomic::AtomicBool::new(false),
        menu_devices: Mutex::new(Vec::new()),
        menu_incoming: Mutex::new(Vec::new()),
    });
    unsafe {
        let inst = GetModuleHandleW(null());
        let class = wide("FerryTray");
        let wc = WNDCLASSEXW {
            cb_size: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            wnd_proc,
            cls_extra: 0,
            wnd_extra: 0,
            instance: inst,
            icon: 0,
            cursor: 0,
            background: 0,
            menu_name: null(),
            class_name: class.as_ptr(),
            icon_sm: 0,
        };
        RegisterClassExW(&wc);
        let title = wide("Ferry");
        let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), 0, 0, 0, 0, 0, 0, 0, inst, null_mut());
        if hwnd == 0 {
            return Err(std::io::Error::other("cannot create the tray window"));
        }
        if g.icon.load(Ordering::SeqCst) == 0 {
            g.icon.store(make_icon(), Ordering::SeqCst);
        }
        g.hwnd.store(hwnd, Ordering::SeqCst);
        if g.icon.load(Ordering::SeqCst) == 0 {
            eprintln!("ferry: could not create the tray icon image");
        }
        let tc = wide("TaskbarCreated");
        g.taskbar_created.store(RegisterWindowMessageW(tc.as_ptr()), Ordering::SeqCst);
        // Right after login the taskbar may not be ready yet: retry for up to 60 s.
        let mut added = false;
        for _ in 0..120 {
            if tray_add(g) {
                added = true;
                break;
            }
            Sleep(500);
        }
        eprintln!("ferry: tray icon {}", if added { "added" } else { "could NOT be added" });
        if AddClipboardFormatListener(hwnd) == 0 {
            eprintln!("ferry: clipboard listener could not be registered");
        }
        PostMessageW(hwnd, WM_APP_EVENT, 0, 0); // events queued before the window existed

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let d = nid(g);
        Shell_NotifyIconW(NIM_DELETE, &d);
    }
    Ok(())
}
