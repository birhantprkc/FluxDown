//! Windows `.torrent` file association via HKCU registry.
//!
//! Registry structure (matches the Inno Setup installer):
//! ```text
//! HKCU\Software\Classes\.torrent                               → "FluxDown.TorrentFile"
//! HKCU\Software\Classes\FluxDown.TorrentFile                   → "BitTorrent File"
//! HKCU\Software\Classes\FluxDown.TorrentFile\DefaultIcon       → "<exe>,0"
//! HKCU\Software\Classes\FluxDown.TorrentFile\shell\open\command → "\"<exe>\" \"%1\""
//! ```
//!
//! All operations target `HKEY_CURRENT_USER` — no admin elevation required.

#[cfg(target_os = "windows")]
mod inner {
    use std::io;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;

    const PROG_ID: &str = "FluxDown.TorrentFile";
    const PROG_DESC: &str = "BitTorrent File";
    const EXT: &str = ".torrent";

    /// Get the canonical path of the current running executable.
    ///
    /// Uses `std::fs::canonicalize` to resolve symlinks and `\\?\` prefixes,
    /// then strips the `\\?\` prefix (if any) for clean comparison with
    /// registry values written by `associate()`.
    fn exe_path() -> Result<String, io::Error> {
        let path = std::env::current_exe()?;
        // canonicalize resolves symlinks and normalizes the path, but on
        // Windows it may add a `\\?\` extended-length prefix.
        let canonical = std::fs::canonicalize(&path).unwrap_or(path);
        let s = canonical.to_string_lossy().into_owned();
        // Strip the extended-length prefix for clean registry comparison.
        Ok(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
    }

    /// Check whether `.torrent` files are currently associated with FluxDown.
    ///
    /// Returns `true` if `HKCU\Software\Classes\.torrent` default value
    /// equals `"FluxDown.TorrentFile"`. We intentionally do NOT compare the
    /// exe path in the command, because path representations can differ
    /// between the installer and the running process (UNC prefix, casing,
    /// short names, etc.). Checking the ProgID alone is sufficient to
    /// confirm FluxDown owns the association.
    pub fn is_associated() -> bool {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // Check .torrent → FluxDown.TorrentFile
        let ext_key =
            match hkcu.open_subkey_with_flags(format!("Software\\Classes\\{EXT}"), KEY_READ) {
                Ok(k) => k,
                Err(_) => return false,
            };
        let prog_id: String = match ext_key.get_value("") {
            Ok(v) => v,
            Err(_) => return false,
        };
        prog_id == PROG_ID
    }

    /// Register `.torrent` file association with FluxDown.
    pub fn associate() -> Result<(), io::Error> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let exe = exe_path()?;

        // 1. .torrent → FluxDown.TorrentFile
        let (ext_key, _) =
            hkcu.create_subkey_with_flags(format!("Software\\Classes\\{EXT}"), KEY_WRITE)?;
        ext_key.set_value("", &PROG_ID)?;

        // 2. FluxDown.TorrentFile description
        let (prog_key, _) =
            hkcu.create_subkey_with_flags(format!("Software\\Classes\\{PROG_ID}"), KEY_WRITE)?;
        prog_key.set_value("", &PROG_DESC)?;

        // 3. DefaultIcon
        let (icon_key, _) = hkcu.create_subkey_with_flags(
            format!("Software\\Classes\\{PROG_ID}\\DefaultIcon"),
            KEY_WRITE,
        )?;
        icon_key.set_value("", &format!("\"{exe}\",0"))?;

        // 4. shell\open\command
        let (cmd_key, _) = hkcu.create_subkey_with_flags(
            format!("Software\\Classes\\{PROG_ID}\\shell\\open\\command"),
            KEY_WRITE,
        )?;
        cmd_key.set_value("", &format!("\"{exe}\" \"%1\""))?;

        // Notify the shell about the change
        notify_shell();

        rinf::debug_print!("[file_assoc] associated .torrent with FluxDown (exe={exe})");
        Ok(())
    }

    /// Remove `.torrent` file association for FluxDown.
    pub fn disassociate() -> Result<(), io::Error> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // Only remove if currently associated to us (don't break other app's association)
        if !is_associated() {
            rinf::debug_print!("[file_assoc] not associated to FluxDown, skipping removal");
            return Ok(());
        }

        // Remove .torrent key
        let classes = hkcu.open_subkey_with_flags("Software\\Classes", KEY_WRITE)?;
        let _ = classes.delete_subkey_all(EXT);

        // Remove FluxDown.TorrentFile tree
        let _ = classes.delete_subkey_all(PROG_ID);

        // Notify the shell about the change
        notify_shell();

        rinf::debug_print!("[file_assoc] removed .torrent association");
        Ok(())
    }

    /// Call SHChangeNotify to inform Explorer about file association changes.
    ///
    /// Uses raw FFI to avoid pulling in the `Win32_UI_Shell` feature gate
    /// of `windows-sys`.
    fn notify_shell() {
        // SHCNE_ASSOCCHANGED = 0x08000000, SHCNF_IDLIST = 0x0000
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn SHChangeNotify(
                wEventId: i32,
                uFlags: u32,
                dwItem1: *const std::ffi::c_void,
                dwItem2: *const std::ffi::c_void,
            );
        }
        unsafe {
            SHChangeNotify(0x08000000, 0, std::ptr::null(), std::ptr::null());
        }
    }
}

// Non-Windows stubs — file association is Windows-only.
#[cfg(not(target_os = "windows"))]
mod inner {
    use std::io;

    pub fn is_associated() -> bool {
        false
    }

    pub fn associate() -> Result<(), io::Error> {
        Ok(())
    }

    pub fn disassociate() -> Result<(), io::Error> {
        Ok(())
    }
}

pub use inner::{associate, disassociate, is_associated};
