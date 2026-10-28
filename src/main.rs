#![windows_subsystem = "windows"]

//! seen — CLI/installer for the seenmark shell overlay.
//! Run from a terminal: stdout is inherited; launched from the Explorer
//! context menu it shows no console window.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use windows::core::{Error, HSTRING};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use windows::Win32::UI::Shell::{
    ILCreateFromPathW, SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNE_UPDATEITEM, SHCNF_IDLIST,
};

use seenmark::{ico, reg, store, CLSID_OVERLAY_STR};

mod watch;

const MENU_KEY: &str = "SeenMarkToggle";
const SLOT_KEY: &str = "001SeenMark";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("seen: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let machine = args.iter().any(|a| a == "--machine");

    match cmd {
        "install" => install(machine)?,
        "uninstall" => uninstall(machine),
        "mark" | "unmark" | "toggle" => {
            let path = args.get(1).ok_or("missing <path>")?;
            let key = canon(path);
            let mut map = store::try_load().map_err(|e| format!("store is corrupt: {e}"))?;
            let cur = map.get(&key).copied().unwrap_or(0);
            let marked = match cmd {
                "mark" => true,
                "unmark" => false,
                _ => cur <= 0,
            };
            if marked {
                map.insert(key.clone(), store::now_secs() as i64);
            } else if cur > 0 {
                // tombstone = minus the unmark time (diagnostic only;
                // the scan treats any tombstone as final)
                map.insert(key.clone(), -(store::now_secs() as i64));
            } else {
                println!("already unmarked: {key}");
                return Ok(());
            }
            store::save(&map)?;
            notify_item(&key);
            println!(
                "{}: {}",
                if marked { "marked as watched" } else { "unmarked" },
                key
            );
        }
        "list" => {
            let map = store::load();
            let mut items: Vec<(i64, &str)> = map
                .iter()
                .filter(|(_k, v)| **v > 0)
                .map(|(k, v)| (*v, k.as_str()))
                .collect();
            items.sort();
            if items.is_empty() {
                println!("no marks");
            }
            for (_, k) in items {
                println!("{k}");
            }
        }
        "where" => {
            println!("data: {}", store::data_dir().display());
            println!("state: {}", store::state_path().display());
            println!("icon: {}", store::icon_path().display());
            println!("bin: {}", store::bin_dir().display());
            println!("config: {}", watch::config_path().display());
        }
        "extract" => {
            let f = args.get(1).ok_or("missing <file>")?;
            let blob = fs::read(f)?;
            let cfg = watch::load_config_pub();
            for p in watch::extract_paths_pub(&blob, &cfg) {
                println!("{p}");
            }
        }
        "icon" => {
            fs::write(store::icon_path(), ico::seen_ico_bytes())?;
            println!("icon updated: {}", store::icon_path().display());
            unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
        }
        "watch" => watch::watch(args.iter().any(|a| a == "--verbose"))?,
        "watch-stop" => watch::stop()?,
        "dump" => {
            let pid: u32 = args.get(1).and_then(|s| s.parse().ok()).ok_or("missing <pid>")?;
            match watch::process_details(pid) {
                Some((img, cmd)) => {
                    println!("image: {img}");
                    println!("cmd  : {cmd}");
                }
                None => println!("failed to read PEB (pid {pid})"),
            }
        },
        "autostart" => {
            let on = args.get(1).map(String::as_str) == Some("on");
            watch::autostart(on)?;
        }
        _ => {
            println!(
                "usage: seen install [--machine] | uninstall [--machine] | mark|unmark|toggle <path> | list | where\n\
                 \x20      seen watch [--verbose] | watch-stop | autostart on|off"
            );
        }
    }
    Ok(())
}

/// Absolute, normalized, lowercased path (matches what the overlay sees).
fn canon(path: &str) -> String {
    let pb = PathBuf::from(path);
    let full = match pb.canonicalize() {
        Ok(v) => v.to_string_lossy().into_owned(),
        Err(_) => pb.to_string_lossy().into_owned(),
    };
    store::normalize_str(&full)
}

fn hive(machine: bool) -> (HKEY, &'static str, &'static str) {
    if machine {
        (
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\Classes",
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\ShellIconOverlayIdentifiers",
        )
    } else {
        (
            HKEY_CURRENT_USER,
            "Software\\Classes",
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\ShellIconOverlayIdentifiers",
        )
    }
}

fn install(machine: bool) -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let src_dir = exe.parent().ok_or("no exe dir")?.to_path_buf();

    fs::create_dir_all(store::bin_dir())?;
    let dst_dll = store::bin_dir().join("seenmark.dll");
    // A loaded DLL cannot be overwritten, but it can be renamed away.
    if dst_dll.exists() {
        let _ = fs::rename(&dst_dll, store::bin_dir().join("seenmark.old.dll"));
    }
    fs::copy(src_dir.join("seenmark.dll"), &dst_dll)?;
    fs::copy(exe, store::bin_dir().join("seen.exe"))?;
    fs::write(store::icon_path(), ico::seen_ico_bytes())?;

    let dll = store::bin_dir().join("seenmark.dll");
    let seen = store::bin_dir().join("seen.exe");
    let icon = store::icon_path();
    let guid_braced = format!("{{{CLSID_OVERLAY_STR}}}");
    let (root, classes, overlay_root) = hive(machine);

    reg::set_sz(root, &format!("{classes}\\CLSID\\{guid_braced}"), None, "SeenMark Overlay")
        .map_err(win_err)?;
    reg::set_sz(
        root,
        &format!("{classes}\\CLSID\\{guid_braced}\\InprocServer32"),
        None,
        dll.to_str().ok_or("bad dll path")?,
    )
    .map_err(win_err)?;
    reg::set_sz(
        root,
        &format!("{classes}\\CLSID\\{guid_braced}\\InprocServer32"),
        Some("ThreadingModel"),
        "Apartment",
    )
    .map_err(win_err)?;
    reg::set_sz(
        root,
        &format!("{overlay_root}\\{SLOT_KEY}"),
        None,
        &guid_braced,
    )
    .map_err(win_err)?;

    // Explorer context menu entry (toggle)
    let menu = format!("{classes}\\*\\shell\\{MENU_KEY}");
    reg::set_sz(root, &menu, None, "Mark as watched").map_err(win_err)?;
    reg::set_sz(root, &menu, Some("Icon"), icon.to_str().ok_or("bad icon path")?)
        .map_err(win_err)?;
    reg::set_sz(
        root,
        &format!("{menu}\\command"),
        None,
        &format!("\"{}\" toggle \"%1\"", seen.display()),
    )
    .map_err(win_err)?;

    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };

    println!("installed ({}).", if machine { "HKLM" } else { "HKCU" });
    println!("data: {}", store::data_dir().display());
    println!("restart Explorer to make the overlay appear:");
    println!("  taskkill /f /im explorer.exe & start explorer.exe");
    Ok(())
}

fn uninstall(machine: bool) {
    let (root, classes, overlay_root) = hive(machine);
    let guid_braced = format!("{{{CLSID_OVERLAY_STR}}}");
    let _ = reg::delete_tree(root, &format!("{classes}\\CLSID\\{guid_braced}"));
    let _ = reg::delete_tree(root, &format!("{overlay_root}\\{SLOT_KEY}"));
    let _ = reg::delete_tree(root, &format!("{classes}\\*\\shell\\{MENU_KEY}"));
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    println!("uninstalled ({}). restart Explorer.", if machine { "HKLM" } else { "HKCU" });
}

fn win_err(e: Error) -> Box<dyn std::error::Error> {
    format!("registry: {e:?}").into()
}

/// Ask Explorer to redraw this one item right now.
fn notify_item(path: &str) {
    unsafe {
        let pidl = ILCreateFromPathW(&HSTRING::from(path));
        if !pidl.is_null() {
            SHChangeNotify(
                SHCNE_UPDATEITEM,
                SHCNF_IDLIST,
                Some(pidl as *const core::ffi::c_void),
                None,
            );
            CoTaskMemFree(Some(pidl as *const core::ffi::c_void));
        }
    }
}
