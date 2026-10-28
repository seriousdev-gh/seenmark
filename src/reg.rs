use windows::core::{Error, HSTRING, PCWSTR};
use windows::Win32::System::Registry::*;

/// Write a REG_SZ value (creates the key when missing).
pub fn set_sz(root: HKEY, subkey: &str, name: Option<&str>, data: &str) -> Result<(), Error> {
    unsafe {
        let sk = HSTRING::from(subkey);
        let mut h = HKEY::default();
        let sam = REG_SAM_FLAGS(KEY_SET_VALUE.0 | KEY_CREATE_SUB_KEY.0 | KEY_WOW64_64KEY.0);
        RegCreateKeyExW(
            root,
            &sk,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            sam,
            None,
            &mut h,
            None,
        )
        .ok()?;
        let bytes: Vec<u8> = data
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let r = match name {
            Some(n) => {
                let vn = HSTRING::from(n);
                RegSetValueExW(h, &vn, None, REG_SZ, Some(&bytes))
            }
            None => RegSetValueExW(h, PCWSTR::null(), None, REG_SZ, Some(&bytes)),
        };
        let _ = RegCloseKey(h);
        r.ok()
    }
}

/// Delete a key and everything under it.
pub fn delete_tree(root: HKEY, subkey: &str) -> Result<(), Error> {
    unsafe {
        let sk = HSTRING::from(subkey);
        RegDeleteTreeW(root, &sk).ok()
    }
}

/// Delete a single value under a key.
pub fn delete_value(root: HKEY, subkey: &str, name: &str) -> Result<(), Error> {
    unsafe {
        let sk = HSTRING::from(subkey);
        let mut h = HKEY::default();
        RegOpenKeyExW(
            root,
            &sk,
            None,
            KEY_SET_VALUE | REG_SAM_FLAGS(KEY_WOW64_64KEY.0),
            &mut h,
        )
        .ok()?;
        let vn = HSTRING::from(name);
        let r = RegDeleteValueW(h, &vn);
        let _ = RegCloseKey(h);
        r.ok()
    }
}
