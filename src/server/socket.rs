use std::os::unix::fs::DirBuilderExt;

/// Socket directory with proper permissions (0o700, created atomically)
pub fn socket_dir() -> std::path::PathBuf {
    let uid = nix::unistd::getuid();
    let dir = std::path::PathBuf::from(format!("/tmp/retach-{}", uid));
    // Create directory with 0o700 atomically — no TOCTOU window
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Directory exists — verify it's owned by us and has correct permissions
            if let Ok(meta) = std::fs::metadata(&dir) {
                use std::os::unix::fs::MetadataExt;
                if meta.uid() != uid.as_raw() {
                    tracing::error!(
                        path = ?dir,
                        owner = meta.uid(),
                        expected = uid.as_raw(),
                        "socket directory owned by another user — possible symlink attack"
                    );
                }
                // Fix permissions if they drifted
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o777 != 0o700 {
                    if let Err(e) = std::fs::set_permissions(
                        &dir,
                        std::fs::Permissions::from_mode(0o700),
                    ) {
                        tracing::warn!(error = %e, "failed to fix socket directory permissions");
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, path = ?dir, "failed to create socket directory");
        }
    }
    dir
}

/// Return the full path to the server's Unix domain socket.
pub fn socket_path() -> std::path::PathBuf {
    socket_dir().join("retach.sock")
}
