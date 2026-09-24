//! `glance explorer install`: "Open in Glance" on folders in Explorer.
//!
//! Two verbs under `HKEY_CURRENT_USER\Software\Classes`, so no admin rights:
//! one for right clicking a folder, one for right clicking the empty space
//! inside one. Both run `glancew.exe` from beside this binary. On Windows 11
//! they sit under "Show more options"; the new short menu only takes
//! packaged apps.

use std::path::Path;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

const FOLDER: &str = r"Software\Classes\Directory\shell\Glance";
const BACKGROUND: &str = r"Software\Classes\Directory\Background\shell\Glance";
const LABEL: &str = "Open in Glance";

/// Every key to create and its default value. `%1` is the folder clicked,
/// `%V` the folder whose background was clicked.
pub fn entries(glancew: &Path) -> Vec<(String, String)> {
    let exe = glancew.display();
    vec![
        (FOLDER.to_string(), LABEL.to_string()),
        (format!(r"{FOLDER}\command"), format!(r#""{exe}" "%1""#)),
        (BACKGROUND.to_string(), LABEL.to_string()),
        (format!(r"{BACKGROUND}\command"), format!(r#""{exe}" "%V""#)),
    ]
}

pub fn install(glancew: &Path) -> Result<(), String> {
    if !glancew.is_file() {
        return Err(format!(
            "{} not found (build it with `cargo build`)",
            glancew.display()
        ));
    }
    for (key, value) in entries(glancew) {
        set_default(&key, &value).map_err(|e| format!("{key}: {e}"))?;
    }
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    for key in [FOLDER, BACKGROUND] {
        let e = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
        // Already gone is fine.
        if e != ERROR_SUCCESS && e.0 != 2 {
            return Err(format!("{key}: error {}", e.0));
        }
    }
    Ok(())
}

pub fn is_installed() -> bool {
    [FOLDER, BACKGROUND].iter().all(|key| {
        let mut h = HKEY::default();
        let e = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(format!(r"{key}\command")),
                None,
                KEY_READ,
                &mut h,
            )
        };
        if e == ERROR_SUCCESS {
            unsafe {
                let _ = RegCloseKey(h);
            }
        }
        e == ERROR_SUCCESS
    })
}

fn set_default(key: &str, value: &str) -> Result<(), String> {
    let mut h = HKEY::default();
    unsafe {
        let e = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut h,
            None,
        );
        if e != ERROR_SUCCESS {
            return Err(format!("error {}", e.0));
        }
        let data: Vec<u8> = value
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        let e = RegSetValueExW(h, PCWSTR::null(), None, REG_SZ, Some(&data));
        let _ = RegCloseKey(h);
        if e != ERROR_SUCCESS {
            return Err(format!("error {}", e.0));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbs_quote_the_binary_and_the_folder() {
        let e = entries(Path::new(r"C:\Program Files\Glance\glancew.exe"));
        assert_eq!(e.len(), 4);
        assert_eq!(
            e[1],
            (
                r"Software\Classes\Directory\shell\Glance\command".to_string(),
                r#""C:\Program Files\Glance\glancew.exe" "%1""#.to_string()
            )
        );
        assert!(e[3].1.ends_with(r#""%V""#));
        assert_eq!(e[2].1, "Open in Glance");
    }
}
