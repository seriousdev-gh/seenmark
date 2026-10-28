//! Auto-mark: poll for media-player process launches, read their command
//! line from the PEB, and mark every media file passed to them.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::slice;

use serde::{Deserialize, Serialize};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::ProcessStatus::K32EnumProcesses;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, PROCESS_VM_READ,
};
use windows::Win32::UI::Shell::CommandLineToArgvW;

use seenmark::store;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "SeenMark";

#[derive(Serialize, Deserialize)]
pub struct Config {
    /// lowercase exe names whose launches are inspected
    pub processes: Vec<String>,
    /// lowercase file extensions treated as media
    pub extensions: Vec<String>,
    /// poll interval, ms
    pub poll_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            processes: [
                "mpc-be64.exe",
                "mpc-be.exe",
                "mpc-hc64.exe",
                "mpc-hc.exe",
                "mpv.exe",
                "mpv-net.exe",
                "mpvqt.exe",
                "potplayermini64.exe",
                "potplayermini.exe",
                "potplayer64.exe",
                "potplayer.exe",
                "vlc.exe",
                "mpv-player.exe",
                "kodi.exe",
                "mplayer2.exe",
                "ffplay.exe",
            ]
            .map(String::from)
            .to_vec(),
            extensions: [
                "mkv", "mk3d", "mp4", "m4v", "avi", "mov", "webm", "flv", "wmv", "ts", "m2ts",
                "mpg", "mpeg", "mp3", "m4a", "flac", "opus", "ogg", "ogv", "wav", "aac",
            ]
            .map(String::from)
            .to_vec(),
            poll_ms: 400,
        }
    }
}

pub fn config_path() -> PathBuf {
    store::data_dir().join("config.json")
}

pub fn load_config_pub() -> Config {
    load_config()
}

pub fn extract_paths_pub(blob: &[u8], cfg: &Config) -> Vec<String> {
    extract_paths(blob, cfg)
}

fn load_config() -> Config {
    let p = config_path();
    match fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => {
            let cfg = Config::default();
            if let Ok(s) = serde_json::to_string_pretty(&cfg) {
                let _ = fs::write(&p, s);
            }
            cfg
        }
    }
}

// ---- ntdll bits not in the windows crate ---------------------------------

#[repr(C)]
struct ProcessBasicInformation {
    exit_status: i32,
    peb_base_address: *mut u8,
    affinity_mask: usize,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryInformationProcess(
        process: HANDLE,
        class: i32,
        info: *mut ProcessBasicInformation,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
}

const PROCESS_BASIC_INFORMATION_CLASS: i32 = 0;

// x64 offsets
const PEB_PROCESS_PARAMETERS: usize = 0x20;
const UPP_COMMAND_LINE: usize = 0x70; // RTL_USER_PROCESS_PARAMETERS.CommandLine
const UPP_IMAGE_PATH_NAME: usize = 0x30;
const UNICODE_STRING_BUFFER: usize = 0x08;

fn read_mem<T>(proc: HANDLE, addr: usize) -> Option<T> {
    let mut out = core::mem::MaybeUninit::<T>::uninit();
    let mut done = 0usize;
    unsafe {
        ReadProcessMemory(
            proc,
            addr as *const core::ffi::c_void,
            out.as_mut_ptr().cast(),
            core::mem::size_of::<T>(),
            Some(&mut done),
        )
        .ok()?;
        if done != core::mem::size_of::<T>() {
            return None;
        }
        Some(out.assume_init())
    }
}

/// Reads a x64 UNICODE_STRING living in the target process.
fn read_u16_str(proc: HANDLE, us_addr: usize) -> Option<String> {
    let len = read_mem::<u16>(proc, us_addr)? as usize;
    if len == 0 || len > 32768 {
        return None;
    }
    let buf = read_mem::<usize>(proc, us_addr + UNICODE_STRING_BUFFER)?;
    if buf == 0 {
        return None;
    }
    let mut v = vec![0u16; len / 2 + 1];
    let mut done = 0usize;
    unsafe {
        ReadProcessMemory(
            proc,
            buf as *const core::ffi::c_void,
            v.as_mut_ptr().cast(),
            len,
            Some(&mut done),
        )
        .ok()?;
    }
    String::from_utf16(&v[..len / 2]).ok()
}

/// (image path, command line) of a live process, x64. None for Wow64/killed.
pub fn process_details(pid: u32) -> Option<(String, String)> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
        let _guard = HandleGuard(h);
        let pbi = {
            let mut pbi = core::mem::zeroed::<ProcessBasicInformation>();
            let mut got = 0u32;
            let st = NtQueryInformationProcess(
                h,
                PROCESS_BASIC_INFORMATION_CLASS,
                &mut pbi,
                core::mem::size_of::<ProcessBasicInformation>() as u32,
                &mut got,
            );
            if st >= 0 {
                pbi
            } else {
                return None;
            }
        };
        let peb = pbi.peb_base_address;
        if peb.is_null() {
            return None;
        }
        let params = read_mem::<usize>(h, peb as usize + PEB_PROCESS_PARAMETERS)?;
        if params == 0 {
            return None;
        }
        let cmd = read_u16_str(h, params + UPP_COMMAND_LINE)?;
        let img = read_u16_str(h, params + UPP_IMAGE_PATH_NAME)
            .unwrap_or_else(|| exe_path(pid).unwrap_or_default());
        Some((img, cmd))
    }
}

struct HandleGuard(HANDLE);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn exe_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let _g = HandleGuard(h);
        let mut buf = [0u16; 4096];
        let mut len = buf.len() as u32;
        QueryFullProcessImageNameW(h, Default::default(), PWSTR(buf.as_mut_ptr()), &mut len)
            .ok()
            .map(|_| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn enum_pids() -> Vec<u32> {
    let mut v = vec![0u32; 8192];
    let mut got = 0u32;
    unsafe {
        if K32EnumProcesses(v.as_mut_ptr(), (v.len() * 4) as u32, &mut got).as_bool() {
            v.truncate((got as usize) / 4);
            v
        } else {
            Vec::new()
        }
    }
}

/// Splits a command line into argv the same way Windows does.
fn split_cmdline(cmd: &str) -> Vec<String> {
    let wide: Vec<u16> = cmd.encode_utf16().chain([0]).collect();
    let mut argc = 0i32;
    unsafe {
        let argv = CommandLineToArgvW(PCWSTR(wide.as_ptr()), &mut argc);
        if argv.is_null() {
            return vec![cmd.to_string()];
        }
        let out = slice::from_raw_parts(argv, argc.max(0) as usize)
            .iter()
            .map(|s| s.to_string().unwrap_or_default())
            .collect();
        let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(argv as *mut _)));
        out
    }
}

fn extract_media(cmd: &str, cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    for arg in split_cmdline(cmd).into_iter().skip(1) {
        let pb = PathBuf::from(&arg);
        let ext = match pb.extension().and_then(|e| e.to_str()) {
            Some(e) => e.to_ascii_lowercase(),
            None => continue,
        };
        if !cfg.extensions.contains(&ext) {
            continue;
        }
        // URLs and nonexistent paths are not files we can mark
        if arg.contains("://") || !pb.is_file() {
            continue;
        }
        out.push(arg);
    }
    out
}

fn mark(path: &str) {
    let key = crate::canon(path);
    let mut map = match store::try_load() {
        Ok(m) => m,
        Err(e) => {
            log_line(true, &format!("mark skipped, store unreadable: {e}"));
            return;
        }
    };
    if map.get(&key).copied().unwrap_or(0) > 0 {
        return; // already watched — no rewrite, no notify spam
    }
    // fresh mark, or the user opened the file again after an unmark:
    // an explicit player launch always wins over a tombstone
    map.insert(key.clone(), store::now_secs() as i64);
    let _ = store::save(&map);
    crate::notify_item(&key);
}

fn log_line(verbose: bool, msg: &str) {
    if !verbose {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store::data_dir().join("watch.log"))
    {
        let _ = writeln!(f, "{msg}");
    }
}

pub fn watch(verbose: bool) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = load_config();
    let players: HashSet<String> = cfg.processes.iter().map(|s| s.to_ascii_lowercase()).collect();
    let my_pid = unsafe { GetCurrentProcessId() };

    // Processes alive at startup are considered pre-existing: not marked.
    let mut known: HashSet<u32> = enum_pids().into_iter().collect();
    log_line(verbose, &format!("watch start; {} known pids", known.len()));

    // jump-list scan: every ~5s
    let mut seen_dest: std::collections::HashMap<PathBuf, u64> = Default::default();
    let mut last_scan = std::time::Instant::now();

    loop {
        std::thread::sleep(std::time::Duration::from_millis(cfg.poll_ms.max(100)));
        if last_scan.elapsed().as_secs() >= 5 {
            last_scan = std::time::Instant::now();
            if scan_autodest(&mut seen_dest, &cfg) {
                log_line(verbose, "autodest scan: new marks");
            }
        }
        for pid in enum_pids() {
            if pid == 0 || pid == my_pid || known.contains(&pid) {
                continue;
            }
            known.insert(pid);
            let path = match exe_path(pid) {
                Some(p) => p,
                None => continue,
            };
            let name = match path.rsplit(['\\', '/']).next() {
                Some(n) => n.to_ascii_lowercase(),
                None => continue,
            };
            if !players.contains(&name) {
                continue;
            }
            // The process may exit right after launching (DDE hand-off);
            // give the PEB read a couple of tries.
            for _ in 0..3 {
                if let Some((_img, cmd)) = process_details(pid) {
                    for f in extract_media(&cmd, &cfg) {
                        log_line(verbose, &format!("player {name} pid {pid}: {f}"));
                        mark(&f);
                    }
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        }
    }
}

// ---- automatic destinations (jump lists) scan -----------------------------
//
// Every ShellExecute'd document (Explorer double-click, including DDE
// hand-offs like MPC-BE single-instance) is recorded by Windows in
// %APPDATA%\Microsoft\Windows\Recent\AutomaticDestinations\*.
// The blobs embed absolute UTF-16LE paths; we extract media ones and mark.

fn autodest_dir() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Recent")
        .join("AutomaticDestinations")
}

/// Find candidate absolute paths with media extensions in a binary blob.
fn extract_paths(blob: &[u8], cfg: &Config) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for align in [0usize, 1] {
        let units: Vec<u16> = blob[align..]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let exts: Vec<Vec<u16>> = cfg
            .extensions
            .iter()
            .map(|e| format!(".{e}").encode_utf16().collect())
            .collect();
        let mut i = 0usize;
        while i < units.len() {
            let mut ext_len = 0usize;
            for ext in &exts {
                // ASCII-case-insensitive: blobs store ".MP4" as often as ".mp4"
                let rest = &units[i..];
                if rest.len() >= ext.len()
                    && rest[..ext.len()]
                        .iter()
                        .zip(ext)
                        .all(|(a, b)| *a == *b || (*a | 0x20) == *b)
                {
                    ext_len = ext.len();
                    break;
                }
            }
            if ext_len == 0 {
                i += 1;
                continue;
            }
            let ext_end = i + ext_len;
            // walk backwards over printable path chars
            let mut start = i;
            while start > 0 && ext_end - start < 1024 {
                let c = units[start - 1];
                if c < 0x20
                    || char::from_u32(c as u32).map(|ch| ch.is_control()).unwrap_or(true)
                {
                    break;
                }
                start -= 1;
            }
            let s: String = String::from_utf16_lossy(&units[start..ext_end]);
            // keep from the last plausible drive root
            if let Some(pos) = s.rfind(":\\") {
                let cand = match s.get(pos.saturating_sub(1)..) {
                    Some(c) => c,
                    None => match s.get(pos..) {
                        Some(c) => c,
                        None => {
                            i = ext_end;
                            continue;
                        }
                    },
                };
                let b = cand.as_bytes();
                if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
                    found.push(cand.to_string());
                }
            }
            i = ext_end;
        }
    }
    found.sort();
    found.dedup();
    found
}

fn scan_autodest(seen: &mut std::collections::HashMap<PathBuf, u64>, cfg: &Config) -> bool {
    let dir = autodest_dir();
    let rd = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(_) => return false,
    };
    let mut fresh: Vec<(String, u64)> = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        let md = match entry.metadata() {
            Ok(md) => md,
            Err(_) => continue,
        };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if seen.get(&p).copied() == Some(mtime) {
            continue;
        }
        seen.insert(p.clone(), mtime);
        let blob = match fs::read(&p) {
            Ok(b) => b,
            Err(_) => continue,
        };
        for path in extract_paths(&blob, cfg) {
            fresh.push((path, mtime));
        }
    }
    if fresh.is_empty() {
        return false;
    }
    // reload right before writing to not clobber concurrent CLI/menu changes;
    // on read/parse failure skip the whole cycle — never overwrite what we
    // could not read.
    let mut map = match store::try_load() {
        Ok(m) => m,
        Err(_) => return false,
    };
    let mut changed = false;
    for (path, mtime) in fresh {
        if !PathBuf::from(&path).is_file() {
            continue; // stale jump-list entry for a deleted/renamed file
        }
        let key = crate::canon(&path);
        // jump-list channel never (re-)marks paths the user unmarked:
        // any existing entry, tombstone included, is authoritative
        if !map.contains_key(&key) {
            map.insert(key.clone(), mtime as i64);
            crate::notify_item(&key);
            changed = true;
        }
    }
    if changed {
        let _ = store::save(&map);
    }
    changed
}

pub fn autostart(on: bool) -> Result<(), Box<dyn std::error::Error>> {
    if on {
        let exe = store::bin_dir().join("seen.exe");
        if !exe.exists() {
            return Err("seen.exe не установлен (seen install)".into());
        }
        seenmark::reg::set_sz(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            RUN_KEY,
            Some(RUN_VALUE),
            &format!("\"{}\" watch", exe.display()),
        )
        .map_err(|e| format!("реестр: {e:?}"))?;
        println!("автозапуск включён: {}", format!("\"{}\" watch", exe.display()));
    } else {
        let _ = seenmark::reg::delete_value(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
        );
        println!("автозапуск выключен");
    }
    Ok(())
}

/// Kills background `seen watch` instances (any except ourselves).
pub fn stop() -> Result<(), Box<dyn std::error::Error>> {
    let my = unsafe { GetCurrentProcessId() };
    let mut killed = 0;
    for pid in enum_pids() {
        if pid == my {
            continue;
        }
        let path = match exe_path(pid) {
            Some(p) => p,
            None => continue,
        };
        if !path.to_ascii_lowercase().ends_with("seen.exe") {
            continue;
        }
        if let Some((_, cmd)) = process_details(pid) {
            if !cmd.to_ascii_lowercase().contains("watch") {
                continue;
            }
        } else {
            continue;
        }
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                if TerminateProcess(h, 0).is_ok() {
                    killed += 1;
                }
                let _ = CloseHandle(h);
            }
        }
    }
    println!("остановлено экземпляров: {killed}");
    Ok(())
}
