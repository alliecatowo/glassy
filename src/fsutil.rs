//! Small filesystem helpers shared by config and session persistence.

use std::io::Write;
use std::path::Path;

/// Write `contents` to `path` atomically: write a sibling temp file, fsync it,
/// then `rename` over the destination. A crash or a concurrent reader (the
/// config file watcher!) therefore sees either the old file or the complete new
/// one, never a truncated half-write.
///
/// A symlinked destination (dotfile managers) is written THROUGH: the link is
/// resolved first so the rename replaces the real file and the link survives.
/// An existing file's permissions are carried over to the replacement.
pub(crate) fn atomic_write(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    // Per-process temp name so two glassy instances never share a temp file.
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_ref())?;
        if let Ok(md) = std::fs::metadata(&target) {
            let _ = f.set_permissions(md.permissions());
        }
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("glassy-fsutil-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn replaces_contents_and_leaves_no_temp_files() {
        let d = scratch("replace");
        let p = d.join("glassy.conf");
        atomic_write(&p, "one").unwrap();
        atomic_write(&p, "two-longer").unwrap();
        atomic_write(&p, "3").unwrap(); // shorter: must not leave a stale tail
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "3");
        let names: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "temp file left behind: {names:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_a_symlink() {
        let d = scratch("link");
        let real = d.join("real.conf");
        let link = d.join("link.conf");
        std::fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        atomic_write(&link, "new").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch("perm");
        let p = d.join("session.json");
        std::fs::write(&p, "x").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        atomic_write(&p, "y").unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
