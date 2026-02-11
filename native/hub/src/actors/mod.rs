mod download_actor;

use std::path::PathBuf;

pub async fn create_actors() {
    // Use the executable's directory, NOT the current working directory.
    // When launched via file association (double-clicking a .torrent file),
    // Windows sets CWD to the .torrent file's directory, which would cause
    // flux_down.db to be created next to the torrent file.
    let db_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        });
    download_actor::run(db_dir).await;
}
