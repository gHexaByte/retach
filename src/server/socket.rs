use std::os::unix::fs::DirBuilderExt;

/// Socket directory with proper permissions (0o700, created atomically).
/// Prefers `$XDG_RUNTIME_DIR/retach` (per-user, mode 0700, managed by systemd)
/// and falls back to `/tmp/retach-{uid}`.
pub fn socket_dir() -> anyhow::Result<std::path::PathBuf> {
    let uid = nix::unistd::getuid();
    let dir = if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        std::path::PathBuf::from(xdg).join("retach")
    } else {
        std::path::PathBuf::from(format!("/tmp/retach-{}", uid))
    };
    // Create directory with 0o700 atomically — no TOCTOU window
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Use symlink_metadata (lstat) to detect symlinks — metadata() follows them
            let meta = std::fs::symlink_metadata(&dir)
                .map_err(|e| anyhow::anyhow!("cannot stat socket directory {dir:?}: {e}"))?;
            if meta.file_type().is_symlink() {
                anyhow::bail!(
                    "socket directory {dir:?} is a symlink — possible symlink attack, refusing to start"
                );
            }
            use std::os::unix::fs::MetadataExt;
            if meta.uid() != uid.as_raw() {
                anyhow::bail!(
                    "socket directory {dir:?} owned by uid {} (expected {}) — possible attack",
                    meta.uid(),
                    uid.as_raw(),
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
        Err(e) => {
            return Err(anyhow::anyhow!("failed to create socket directory {dir:?}: {e}"));
        }
    }
    Ok(dir)
}

/// Return the full path to the server's Unix domain socket.
pub fn socket_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(socket_dir()?.join("retach.sock"))
}

/// Return the full path to the server startup lockfile.
pub fn lock_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(socket_dir()?.join("retach.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_dir_returns_correct_format() {
        let uid = nix::unistd::getuid();
        let dir = socket_dir().unwrap();
        if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
            let expected = std::path::PathBuf::from(xdg).join("retach");
            assert_eq!(dir, expected);
        } else {
            let expected = std::path::PathBuf::from(format!("/tmp/retach-{}", uid));
            assert_eq!(dir, expected);
        }
    }

    #[test]
    fn socket_path_ends_with_sock() {
        let path = socket_path().unwrap();
        assert!(
            path.ends_with("retach.sock"),
            "socket_path should end with 'retach.sock', got: {:?}",
            path
        );
    }

    #[test]
    fn socket_dir_creates_directory() {
        let dir = socket_dir().unwrap();
        assert!(
            dir.exists(),
            "socket_dir() should create the directory at {:?}",
            dir
        );
        assert!(
            dir.is_dir(),
            "socket_dir() path should be a directory, not a file"
        );
    }

    #[test]
    fn socket_dir_has_correct_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = socket_dir().unwrap();
        let meta = std::fs::metadata(&dir).expect("should be able to stat socket directory");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "socket directory should have mode 0o700, got: {:#o}",
            mode
        );
    }

    #[test]
    fn socket_dir_idempotent() {
        let first = socket_dir().unwrap();
        let second = socket_dir().unwrap();
        assert_eq!(
            first, second,
            "calling socket_dir() twice should return the same path"
        );
        assert!(
            second.exists(),
            "directory should still exist after second call"
        );
    }
}
