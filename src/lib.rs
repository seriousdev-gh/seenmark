//! seenmark: Windows shell icon overlay handler (Win11 24H2+ IShellIconOverlayIdentifier)
//! + IClassFactory, so the DLL can be hosted by shell32 via CoCreateInstance.

pub mod ico;
pub mod reg;
pub mod store;

use std::collections::HashSet;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::{LazyLock, Mutex};

use windows::core::{
    implement, BOOL, ComObjectInner, Error, GUID, HRESULT, IUnknown, Interface, PCWSTR, PWSTR, Ref,
};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_INVALIDARG, E_POINTER, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::HKEY_CURRENT_USER;
use windows::Win32::UI::Shell::{
    IShellIconOverlayIdentifier, IShellIconOverlayIdentifier_Impl,
};

pub const CLSID_OVERLAY_STR: &str = "6f9d2c41-1a57-4b3e-8c2d-9e5f7ab4c0d9";

pub const CLSID_OVERLAY: GUID = GUID {
    data1: 0x6f9d2c41,
    data2: 0x1a57,
    data3: 0x4b3e,
    data4: [0x8c, 0x2d, 0x9e, 0x5f, 0x7a, 0xb4, 0xc0, 0xd9],
};

const IOIF_ICONFILENAME: u32 = 0x1;

type Cache = (Option<std::time::SystemTime>, HashSet<String>);
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new((None, HashSet::new())));

fn is_watched(path: &str) -> bool {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    match std::fs::metadata(store::state_path()) {
        Ok(md) => {
            let t = md.modified().ok();
            if guard.0 != t {
                if let Ok(map) = store::try_load() {
                    guard.1 = map
                        .into_iter()
                        .filter(|(_k, v)| *v > 0)
                        .map(|(k, _)| k)
                        .collect();
                    guard.0 = t;
                }
                // parse error: keep the last good cache, retry on next call
            }
        }
        Err(_) => {
            guard.0 = None;
            guard.1.clear();
        }
    }
    guard.1.contains(&store::normalize_str(path))
}

#[implement(IShellIconOverlayIdentifier)]
pub struct Overlay;

impl IShellIconOverlayIdentifier_Impl for Overlay_Impl {
    /// Called by the 24H2+ shell with the full path of the item.
    /// S_OK => overlay applies; S_FALSE => no overlay.
    fn IsMemberOf(&self, pwszpath: &PCWSTR, _dwattrib: u32) -> windows::core::Result<()> {
        if pwszpath.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        let path = unsafe { pwszpath.to_string() }.unwrap_or_default();
        if path.is_empty() {
            return Err(Error::from_hresult(S_FALSE));
        }
        if is_watched(&path) {
            Ok(())
        } else {
            Err(Error::from_hresult(S_FALSE))
        }
    }

    fn GetOverlayInfo(
        &self,
        pwsziconfile: PWSTR,
        cchmax: i32,
        pindex: *mut i32,
        pdwflags: *mut u32,
    ) -> windows::core::Result<()> {
        if pwsziconfile.is_null() || pindex.is_null() || pdwflags.is_null() {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        let path = store::icon_path().to_string_lossy().into_owned();
        let wide: Vec<u16> = path.encode_utf16().collect();
        if cchmax <= 0 || wide.len() as i32 >= cchmax {
            return Err(Error::from_hresult(E_INVALIDARG));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(wide.as_ptr(), pwsziconfile.0 as *mut u16, wide.len());
            *pwsziconfile.0.add(wide.len()) = 0;
            *pindex = 0;
            *pdwflags = IOIF_ICONFILENAME;
        }
        Ok(())
    }

    fn GetPriority(&self) -> windows::core::Result<i32> {
        Ok(50)
    }
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<'_, IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if !punkouter.is_null() {
            return Err(Error::from_hresult(CLASS_E_NOAGGREGATION));
        }
        if riid.is_null() || ppvobject.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        unsafe { *ppvobject = null_mut() };
        let obj = Overlay.into_object();
        let unk: IUnknown = obj.cast()?;
        unsafe { unk.query(riid, ppvobject).ok() }
    }

    fn LockServer(&self, _flock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

fn clsid_key(sub: &str) -> String {
    format!("Software\\Classes\\CLSID\\{{{CLSID_OVERLAY_STR}}}{sub}")
}

fn module_path() -> Option<String> {
    let mut buf = [0u16; 4096];
    let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..n]))
}

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() {
        return E_POINTER;
    }
    *ppv = null_mut();
    if rclsid.is_null() || riid.is_null() {
        return E_POINTER;
    }
    if *rclsid != CLSID_OVERLAY {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    // Answer the interface request directly, in addition to IClassFactory,
    // for maximum compatibility with how shell32 instantiates overlays.
    if *riid == IShellIconOverlayIdentifier::IID {
        let obj = Overlay.into_object();
        if let Ok(unk) = obj.cast::<IUnknown>() {
            return unk.query(riid, ppv);
        }
    }
    let fac = Factory.into_object();
    match fac.cast::<IUnknown>() {
        Ok(unk) => unk.query(riid, ppv),
        Err(e) => e.into(),
    }
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    // stay resident; the store cache is worth it
    S_FALSE
}

/// Self-registration (HKCU), usable via regsvr32 as well as `seen install`.
#[no_mangle]
pub unsafe extern "system" fn DllRegisterServer() -> HRESULT {
    let dll = match module_path() {
        Some(p) => p,
        None => return E_INVALIDARG,
    };
    let steps = [
        (clsid_key(""), None, "SeenMark Overlay".to_string()),
        (clsid_key("\\InprocServer32"), None, dll),
        (
            clsid_key("\\InprocServer32"),
            Some("ThreadingModel"),
            "Apartment".to_string(),
        ),
        (
            format!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\ShellIconOverlayIdentifiers\\001SeenMark"
            ),
            None,
            format!("{{{CLSID_OVERLAY_STR}}}"),
        ),
    ];
    for (key, name, data) in steps {
        if reg::set_sz(HKEY_CURRENT_USER, &key, name, &data).is_err() {
            return E_INVALIDARG;
        }
    }
    S_OK
}

#[no_mangle]
pub unsafe extern "system" fn DllUnregisterServer() -> HRESULT {
    let _ = reg::delete_tree(HKEY_CURRENT_USER, &clsid_key(""));
    let _ = reg::delete_tree(
        HKEY_CURRENT_USER,
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\ShellIconOverlayIdentifiers\\001SeenMark",
    );
    S_OK
}
