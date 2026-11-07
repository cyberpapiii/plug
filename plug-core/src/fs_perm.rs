//! Filesystem helpers for secrets: owner-only directories and files.

use std::io::Write as _;
use std::path::Path;

pub const DIR_SYNC_ATTEMPTS: usize = 3;

/// Create `path` (and any missing parents) with mode 0700.
///
/// Only directories this call actually creates get the restrictive mode
/// (`DirBuilder` semantics) — directories that already exist are left
/// untouched, so deliberately customized permissions are preserved.
pub fn ensure_dir_0700(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// Whether a write that replaced its file has reached the disk.
#[derive(Debug)]
#[must_use]
pub enum Durability {
    Durable,
    /// The new file is in place, but syncing its directory failed, so the
    /// rename may not survive a crash.
    Uncertain(std::io::Error),
}

/// Replace `path` with `contents`, readable by its owner only. A reader
/// sees the old file or the new one, never part of either. An error means
/// `path` was left as it was.
///
/// Missing parents are created with mode 0700. The bytes go to a temporary
/// file beside `path`, created exclusively with mode 0600 (so a file or link
/// planted under that name is refused, not written through), synced, and
/// renamed over `path`. The rename replaces a link at `path` instead of
/// following it.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<Durability> {
    replace(path, contents, Some(0o600), ensure_dir_0700)
}

/// Replace a file another program also owns, such as a client's settings,
/// so that a crash or a concurrent reader never sees half of it. The file
/// keeps its permissions; a new one gets the usual ones. A link at `path`
/// is followed, so the file it points at is the one replaced.
pub fn write_atomic_keeping_mode(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let durability = match std::fs::canonicalize(path) {
        Ok(target) => {
            let mode = std::fs::metadata(&target)?.permissions().mode() & 0o7777;
            replace(&target, contents, Some(mode), |dir| {
                std::fs::create_dir_all(dir)
            })?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            replace(path, contents, None, |dir| std::fs::create_dir_all(dir))?
        }
        Err(error) => return Err(error),
    };
    match durability {
        Durability::Durable => Ok(()),
        Durability::Uncertain(error) => Err(error),
    }
}

/// Write `contents` to a temporary file beside `path` and rename it over
/// `path`. `mode` is set exactly; `None` leaves the umask's default.
fn replace(
    path: &Path,
    contents: &[u8],
    mode: Option<u32>,
    make_parent: fn(&Path) -> std::io::Result<()>,
) -> std::io::Result<Durability> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    make_parent(parent)?;
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path names no file")
    })?;
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        // Owner-only until the exact mode is set, so the bytes are never
        // readable by more than the final file allows.
        .mode(if mode.is_some() { 0o600 } else { 0o666 })
        .open(&temporary)
        .and_then(|mut file| {
            if let Some(mode) = mode.filter(|mode| *mode != 0o600) {
                file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            }
            file.write_all(contents)?;
            file.sync_all()
        })
        .and_then(|()| faults::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written?;
    Ok(match sync_dir(parent) {
        Ok(()) => Durability::Durable,
        Err(error) => Durability::Uncertain(error),
    })
}

/// [`write_private`], counting a directory sync that fails as a failed write.
pub fn write_private_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    match write_private(path, contents)? {
        Durability::Durable => Ok(()),
        Durability::Uncertain(error) => Err(error),
    }
}

/// Sync `dir`, so the entries renamed into it survive a crash. Tried
/// [`DIR_SYNC_ATTEMPTS`] times, 10 ms apart.
pub fn sync_dir(dir: &Path) -> std::io::Result<()> {
    let mut attempt = 1;
    loop {
        match faults::sync_dir_once(dir) {
            Ok(()) => return Ok(()),
            Err(error) if attempt == DIR_SYNC_ATTEMPTS => return Err(error),
            Err(_) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}

#[cfg(not(test))]
mod faults {
    use std::path::Path;

    pub(super) fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }

    pub(super) fn sync_dir_once(dir: &Path) -> std::io::Result<()> {
        std::fs::File::open(dir)?.sync_all()
    }
}

/// Failures a test arms on its own thread for the writes it makes next.
#[cfg(test)]
pub(crate) mod faults {
    use std::cell::Cell;
    use std::path::Path;

    std::thread_local! {
        static RENAMES_TO_FAIL: Cell<usize> = const { Cell::new(0) };
        static DIR_SYNCS_TO_FAIL: Cell<usize> = const { Cell::new(0) };
    }

    /// The next private write on this thread fails without replacing its
    /// file.
    pub(crate) fn fail_next_rename() {
        RENAMES_TO_FAIL.set(1);
    }

    /// The next `attempts` directory syncs on this thread fail.
    pub(crate) fn fail_next_dir_syncs(attempts: usize) {
        DIR_SYNCS_TO_FAIL.set(attempts);
    }

    fn take(counter: &'static std::thread::LocalKey<Cell<usize>>) -> bool {
        let remaining = counter.get();
        counter.set(remaining.saturating_sub(1));
        remaining > 0
    }

    pub(super) fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
        if take(&RENAMES_TO_FAIL) {
            return Err(std::io::Error::other("injected rename failure"));
        }
        std::fs::rename(from, to)
    }

    pub(super) fn sync_dir_once(dir: &Path) -> std::io::Result<()> {
        if take(&DIR_SYNCS_TO_FAIL) {
            return Err(std::io::Error::other("injected directory sync failure"));
        }
        std::fs::File::open(dir)?.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shared_file_keeps_its_mode_and_the_link_to_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::paths::test::scratch("keep-mode");
        let target = dir.join("settings.json");
        let link = dir.join("link.json");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_atomic_keeping_mode(&link, b"new").unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o640);
    }

    #[test]
    fn a_shared_file_that_did_not_exist_is_created() {
        let dir = crate::paths::test::scratch("keep-mode-new");
        let path = dir.join("nested/settings.json");
        write_atomic_keeping_mode(&path, b"{}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
    }

    #[test]
    fn a_private_write_replaces_the_file_with_an_owner_only_one() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::paths::test::scratch("write");
        let path = dir.join("token");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_private_atomic(&path, b"new").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp left");
    }

    #[test]
    fn a_private_write_replaces_a_link_instead_of_writing_through_it() {
        let dir = crate::paths::test::scratch("link");
        let target = dir.join("elsewhere");
        std::fs::write(&target, "keep").unwrap();
        let path = dir.join("token");
        std::os::unix::fs::symlink(&target, &path).unwrap();

        write_private_atomic(&path, b"new").unwrap();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        assert!(
            !std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn creates_nested_dir_with_0700_and_is_idempotent() {
        let leaf = crate::paths::test::scratch("fs-perm")
            .join("nested")
            .join("secrets");

        ensure_dir_0700(&leaf).expect("first create succeeds");
        assert!(leaf.is_dir());

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&leaf).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "leaf dir must be created 0700");

        // A second call on the already-existing dir is an Ok no-op.
        ensure_dir_0700(&leaf).expect("second create is Ok");
    }
}
