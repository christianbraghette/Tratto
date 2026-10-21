// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tratto as the system's PDF reader: registers the app, for this user only
//! (no administrator rights), to open PDFs with `tratto --reader <file>`
//! (a reader-only window, see `main`).
//!
//! Windows: the ProgID `Tratto.PDF` under `HKCU\Software\Classes`, offered
//! for `.pdf` in "Open with", and the app's capabilities, listed in
//! `RegisteredApplications` so it shows in Settings > Default apps. Only the
//! user may choose the default there, so `register` is followed by
//! `open_default_apps`.
//! Linux: a desktop entry for `application/pdf` in
//! `~/.local/share/applications`, made the default with `xdg-mime`.
//!
//! The registration names the executable's path: an app moved since (it is
//! a portable folder) shows as not registered, and registering again fixes it.

/// The OS's file associations can be set here.
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "linux"));

/// What `register` leaves to the user (Windows: confirming the default).
pub const REGISTER_HINT: &str = if cfg!(windows) {
    "Windows asks you to confirm it: choose Tratto for .pdf in Settings > Default apps."
} else {
    "Tratto opens PDFs from now on, in a window of their own."
};

/// The running executable, as the registration names it.
fn exe() -> Result<std::path::PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("Cannot find the Tratto executable: {e}"))
}

/// Tratto, as it is now, opens PDFs.
pub fn is_registered() -> bool {
    exe().is_ok_and(|exe| imp::is_registered(&exe))
}

/// Makes Tratto open PDFs (on Windows: offered, see `REGISTER_HINT`).
pub fn register() -> Result<(), String> {
    imp::register(&exe()?).map_err(|e| format!("Cannot register Tratto for PDFs: {e}"))
}

/// Takes Tratto off the apps that open PDFs.
pub fn unregister() -> Result<(), String> {
    imp::unregister(&exe()?).map_err(|e| format!("Cannot unregister Tratto: {e}"))
}

/// Windows: opens Settings > Default apps at Tratto's page.
pub fn open_default_apps() {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg("ms-settings:defaultapps?registeredAppUser=Tratto").spawn();
}

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteKeyW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
    };
    use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

    const PROG_ID: &str = "Tratto.PDF";
    const CLASSES: &str = r"Software\Classes";
    const APP_KEY: &str = r"Software\Tratto";
    const CAPABILITIES: &str = r"Software\Tratto\Capabilities";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn check(code: WIN32_ERROR) -> Result<(), String> {
        if code == ERROR_SUCCESS { Ok(()) } else { Err(std::io::Error::from_raw_os_error(code as i32).to_string()) }
    }

    /// Sets `name` (`None`: the key's default value) of `HKCU\key`, creating the key.
    fn set(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
        let mut hkey: HKEY = null_mut();
        let (key, name, data) = (wide(key), name.map(wide), wide(value));
        // SAFETY: NUL-terminated UTF-16 strings that outlive the calls, and
        // the key opened here is closed before returning.
        unsafe {
            check(RegCreateKeyExW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                null(),
                &mut hkey,
                null_mut(),
            ))?;
            let name = name.as_ref().map_or(null(), |n| n.as_ptr());
            let set = RegSetValueExW(hkey, name, 0, REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32);
            RegCloseKey(hkey);
            check(set)
        }
    }

    /// A string value of `HKCU\key` (`None` name: the default value).
    fn get(key: &str, name: Option<&str>) -> Option<String> {
        let (key, name) = (wide(key), name.map(wide));
        let name = name.as_ref().map_or(null(), |n| n.as_ptr());
        let mut buf = vec![0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: `buf` holds `size` bytes; the strings are NUL-terminated.
        let code = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name,
                RRF_RT_REG_SZ,
                null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        // `size` counts the terminating NUL.
        (code == ERROR_SUCCESS).then(|| String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]))
    }

    /// Ignores what is already gone.
    fn gone(code: WIN32_ERROR) -> Result<(), String> {
        if code == ERROR_FILE_NOT_FOUND { Ok(()) } else { check(code) }
    }

    fn delete_tree(key: &str) -> Result<(), String> {
        // SAFETY: a NUL-terminated UTF-16 string.
        gone(unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(key).as_ptr()) })?;
        gone(unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, wide(key).as_ptr()) })
    }

    fn delete_value(key: &str, name: &str) -> Result<(), String> {
        // SAFETY: NUL-terminated UTF-16 strings.
        gone(unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(key).as_ptr(), wide(name).as_ptr()) })
    }

    fn command(exe: &Path) -> String {
        format!("\"{}\" --reader \"%1\"", exe.display())
    }

    /// `Applications\tratto.exe`: the app as "Open with" lists it.
    fn application_key(exe: &Path) -> String {
        let name = exe.file_name().map_or_else(|| "tratto.exe".into(), |n| n.to_string_lossy());
        format!(r"{CLASSES}\Applications\{name}")
    }

    /// Windows notices the change at once (Explorer's "Open with").
    fn changed() {
        // SAFETY: SHCNE_ASSOCCHANGED takes no items.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null()) };
    }

    pub fn is_registered(exe: &Path) -> bool {
        get(&format!(r"{CLASSES}\{PROG_ID}\shell\open\command"), None).is_some_and(|c| c == command(exe))
    }

    pub fn register(exe: &Path) -> Result<(), String> {
        let (open, icon) = (command(exe), format!("\"{}\",0", exe.display()));
        let prog = format!(r"{CLASSES}\{PROG_ID}");
        set(&prog, None, "PDF document")?;
        set(&format!(r"{prog}\DefaultIcon"), None, &icon)?;
        set(&format!(r"{prog}\shell\open\command"), None, &open)?;
        set(&format!(r"{CLASSES}\.pdf\OpenWithProgids"), Some(PROG_ID), "")?;

        let app = application_key(exe);
        set(&app, Some("FriendlyAppName"), "Tratto")?;
        set(&format!(r"{app}\SupportedTypes"), Some(".pdf"), "")?;
        set(&format!(r"{app}\shell\open\command"), None, &open)?;

        set(CAPABILITIES, Some("ApplicationName"), "Tratto")?;
        set(CAPABILITIES, Some("ApplicationDescription"), env!("CARGO_PKG_DESCRIPTION"))?;
        set(CAPABILITIES, Some("ApplicationIcon"), &icon)?;
        set(&format!(r"{CAPABILITIES}\FileAssociations"), Some(".pdf"), PROG_ID)?;
        set(r"Software\RegisteredApplications", Some("Tratto"), CAPABILITIES)?;
        changed();
        Ok(())
    }

    pub fn unregister(exe: &Path) -> Result<(), String> {
        delete_value(r"Software\RegisteredApplications", "Tratto")?;
        delete_tree(APP_KEY)?;
        delete_value(&format!(r"{CLASSES}\.pdf\OpenWithProgids"), PROG_ID)?;
        delete_tree(&application_key(exe))?;
        delete_tree(&format!(r"{CLASSES}\{PROG_ID}"))?;
        changed();
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const DESKTOP_FILE: &str = "tratto.desktop";

    /// `$XDG_DATA_HOME`, else `~/.local/share`.
    fn data_home() -> Result<PathBuf, String> {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .ok_or_else(|| "no home folder".to_owned())
    }

    fn desktop_path() -> Result<PathBuf, String> {
        Ok(data_home()?.join("applications").join(DESKTOP_FILE))
    }

    fn icon_path() -> Result<PathBuf, String> {
        Ok(data_home()?.join("icons/hicolor/256x256/apps/tratto.png"))
    }

    /// The `Exec` line: the executable quoted as the Desktop Entry spec asks
    /// (`"`, `` ` ``, `$` and `\` escaped inside the quotes, then `\` doubled
    /// as in every string value).
    pub(super) fn exec_line(exe: &Path) -> String {
        let mut quoted = String::new();
        for c in exe.to_string_lossy().chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        format!("Exec=\"{}\" --reader %f", quoted.replace('\\', "\\\\"))
    }

    fn entry(exe: &Path) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=Tratto\nComment={}\n{}\nIcon=tratto\nTerminal=false\n\
             MimeType=application/pdf;\nCategories=Office;Viewer;\n",
            env!("CARGO_PKG_DESCRIPTION"),
            exec_line(exe)
        )
    }

    /// The desktop's database of entries and MIME types (missing tools: the
    /// desktop scans the folder itself).
    fn refresh(dir: &Path) {
        let _ = Command::new("update-desktop-database").arg(dir).status();
    }

    pub fn is_registered(exe: &Path) -> bool {
        desktop_path().and_then(|p| fs::read_to_string(p).map_err(|e| e.to_string())).is_ok_and(|t| t == entry(exe))
    }

    pub fn register(exe: &Path) -> Result<(), String> {
        let (desktop, icon) = (desktop_path()?, icon_path()?);
        let write = |path: &Path, data: &[u8]| -> Result<(), String> {
            fs::create_dir_all(path.parent().unwrap_or(path)).map_err(|e| e.to_string())?;
            fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))
        };
        write(&icon, crate::ICON_PNG)?;
        write(&desktop, entry(exe).as_bytes())?;
        refresh(desktop.parent().unwrap_or(&desktop));
        let status = Command::new("xdg-mime").args(["default", DESKTOP_FILE, "application/pdf"]).status();
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!("xdg-mime failed ({s})")),
            // The entry is there ("Open with" lists Tratto), only the default is not set.
            Err(e) => Err(format!("cannot make it the default app for PDFs (xdg-mime, from xdg-utils: {e})")),
        }
    }

    pub fn unregister(_exe: &Path) -> Result<(), String> {
        let desktop = desktop_path()?;
        for path in [&desktop, &icon_path()?] {
            match fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{}: {e}", path.display())),
                _ => {}
            }
        }
        refresh(desktop.parent().unwrap_or(&desktop));
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use std::path::Path;

    const UNSUPPORTED: &str = "not available on this system";

    pub fn is_registered(_exe: &Path) -> bool {
        false
    }

    pub fn register(_exe: &Path) -> Result<(), String> {
        Err(UNSUPPORTED.to_owned())
    }

    pub fn unregister(_exe: &Path) -> Result<(), String> {
        Err(UNSUPPORTED.to_owned())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::path::Path;

    #[test]
    fn exec_line_quotes_the_path() {
        assert_eq!(super::imp::exec_line(Path::new("/opt/My Apps/tratto")), "Exec=\"/opt/My Apps/tratto\" --reader %f");
        assert_eq!(super::imp::exec_line(Path::new("/a$b/tratto")), "Exec=\"/a\\\\$b/tratto\" --reader %f");
    }
}
