use std::os::unix::fs::DirBuilderExt;

/// Socket directory with proper permissions (0o700, created atomically).
/// Prefers `$XDG_RUNTIME_DIR/retach` (per-user, mode 0700, managed by systemd)
/// and falls back to `/tmp/retach-{uid}`.
pub fn socket_dir() -> anyhow::Result<std::path::PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok().map(std::path::PathBuf::from);
    socket_dir_impl(runtime_dir.as_deref())
}

/// Internal implementation that accepts an optional runtime directory override.
/// Used by tests to avoid mutating the process environment (which is unsound
/// in multi-threaded test runners).
fn socket_dir_impl(runtime_dir: Option<&std::path::Path>) -> anyhow::Result<std::path::PathBuf> {
    let uid = nix::unistd::getuid();
    let dir = if let Some(xdg) = runtime_dir {
        xdg.join("retach")
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

    #[test]
    fn socket_dir_rejects_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real");
        let sym_dir = tmp.path().join("retach");
        std::fs::create_dir(&real_dir).unwrap();
        std::os::unix::fs::symlink(&real_dir, &sym_dir).unwrap();

        // Use socket_dir_impl with an explicit override instead of mutating
        // the process environment, which is unsound in multi-threaded test runners.
        let result = socket_dir_impl(Some(tmp.path()));

        assert!(result.is_err(), "should reject symlink socket directory");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("symlink"), "error should mention symlink: {}", err);
    }

    #[test]
    fn socket_dir_repairs_wrong_permissions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("retach");
        std::fs::DirBuilder::new().mode(0o755).create(&dir).unwrap();

        // Use socket_dir_impl with an explicit override instead of mutating
        // the process environment, which is unsound in multi-threaded test runners.
        let result = socket_dir_impl(Some(tmp.path()));

        assert!(result.is_ok(), "should succeed and repair permissions");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "permissions should be repaired to 0o700, got: {:#o}", mode);
    }

    #[test]
    fn lock_path_format() {
        let path = lock_path().unwrap();
        assert!(path.ends_with("retach.lock"),
            "lock_path should end with 'retach.lock', got: {:?}", path);
    }
}
