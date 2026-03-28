mod download_actor;

use std::path::PathBuf;

pub async fn create_actors() {
    // Determine the database directory.
    //
    // Linux: /opt/fluxdown/ (and similar system install paths) is owned by root
    // and not writable by the user. Use $XDG_DATA_HOME/fluxdown instead
    // (~/.local/share/fluxdown by default), which is always user-writable.
    //
    // macOS: Writing inside the .app bundle (next to the exe) causes code
    // signing failures when Flutter re-signs the bundle. Use the standard
    // macOS application support directory instead:
    // ~/Library/Application Support/fluxdown
    //
    // Windows: use the executable's directory (avoids CWD issues when
    // launched via file association — Windows sets CWD to the .torrent directory).
    #[cfg(target_os = "linux")]
    let db_dir = {
        let base = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".local").join("share")
            });
        let dir = base.join("fluxdown");
        let _ = std::fs::create_dir_all(&dir);
        dir
    };

    #[cfg(target_os = "macos")]
    let db_dir = {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let dir = PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("fluxdown");
        let _ = std::fs::create_dir_all(&dir);
        dir
    };

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let db_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    download_actor::run(db_dir).await;
}
