//! The folders automation may read and write (#1472).
//!
//! `cadcraft-cli mcp --automation-read-root <dir> --automation-write-root <dir>` confines the files
//! an agent's commands reach. [`AutomationRoots`] holds the two folders. While they are in force,
//! every path a command reads goes through [`check_read`] and every path it writes through
//! [`check_write`]:
//!
//! - The path is made absolute against the working directory, as the file system reads it. Its
//!   links are followed: for a file that doesn't exist yet, the links of its nearest existing
//!   folder, the rest of the path being plain names (`..` below a missing folder is refused, and so
//!   is a link that leads nowhere).
//! - The result must lie inside the root, compared by whole components (`/work2` isn't inside
//!   `/work`), without regard to case on Windows.
//! - Reads may lie under either root; writes only under the write root, and never at the write
//!   root itself (its save-style temporary file would be outside). A root given alone grants
//!   reads under it even when it is the write root; a read root grants no writes.
//!
//! Roots are in force on a thread inside [`confine`]. The MCP headless backend runs every call in
//! it; without roots nothing is checked, so the app and the command line keep working everywhere.
//!
//! A path is checked, then opened: a process that can change the folders in between (swapping in
//! a link) can still win that race. The roots keep an agent to the files it was given; they don't
//! guard against other programs on the computer.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Read access or write access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// The folders automation may read and write; a root left out grants no access of its kind.
#[derive(Debug)]
pub struct AutomationRoots {
    read: Option<Root>,
    write: Option<Root>,
}

#[derive(Debug)]
struct Root {
    /// The folder made absolute, links followed: what paths are compared with.
    resolved: PathBuf,
    /// The same path, for people.
    shown: PathBuf,
}

impl Root {
    /// The folder at `path`, which must exist.
    fn open(path: &Path, access: Access) -> Result<Self, String> {
        let kind = match access {
            Access::Read => "read",
            Access::Write => "write",
        };
        let resolved = std::fs::canonicalize(path)
            .and_then(|p| if p.is_dir() { Ok(p) } else { Err(std::io::ErrorKind::NotADirectory.into()) })
            .map_err(|e| format!("cannot open the automation {kind} root `{}`: {e}", path.display()))?;
        Ok(Self { shown: resolved.clone(), resolved })
    }
}

impl AutomationRoots {
    /// The roots `read` and `write`, which must be existing folders; `None` when neither is given
    /// (nothing is confined then).
    pub fn new(read: Option<&Path>, write: Option<&Path>) -> Result<Option<Arc<Self>>, String> {
        if read.is_none() && write.is_none() {
            return Ok(None);
        }
        let read = read.map(|p| Root::open(p, Access::Read)).transpose()?;
        let write = write.map(|p| Root::open(p, Access::Write)).transpose()?;
        Ok(Some(Arc::new(Self { read, write })))
    }

    /// The read root, absolute with its links followed.
    pub fn read_root(&self) -> Option<&Path> {
        self.read.as_ref().map(|r| r.shown.as_path())
    }

    /// The write root, absolute with its links followed.
    pub fn write_root(&self) -> Option<&Path> {
        self.write.as_ref().map(|r| r.shown.as_path())
    }

    /// May the file or folder at `path` be read (or written)? `Err` says why not.
    ///
    /// A read may lie under either root: the read root, or the write root (files the agent may
    /// write it may also read back). A write may lie only under the write root, and not at the
    /// root itself, whose save-style temporary file would sit beside it.
    pub fn check(&self, path: &str, access: Access) -> Result<(), String> {
        self.check_with(path, access, false, resolve)
    }

    fn check_with(
        &self,
        path: &str,
        access: Access,
        directory: bool,
        resolve_path: impl FnOnce(&Path) -> Result<PathBuf, String>,
    ) -> Result<(), String> {
        // Reject remote/device namespaces lexically, before canonicalize can contact a host.
        #[cfg(windows)]
        {
            let normalized = path.replace('/', "\\");
            // A disk-shaped spelling may still normalize to a network/device namespace.
            // Only absolute() below classifies it; mixed separators are not verbatim.
            let disk_spelling = normalized.strip_prefix("\\\\?\\").is_some_and(|p| {
                let mut chars = p.chars();
                chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.next() == Some(':') && chars.next() == Some('\\')
            });
            let special = (normalized.starts_with("\\\\") && !disk_spelling) || normalized.starts_with("\\??\\");
            let extended_network = normalized.to_ascii_lowercase().starts_with("\\\\?\\unc\\");
            if special || extended_network {
                let candidate = if normalized.starts_with("\\\\?\\") || normalized.starts_with("\\\\.\\") || normalized.starts_with("\\??\\") {
                    PathBuf::from(&normalized)
                } else {
                    PathBuf::from(format!("\\\\?\\UNC\\{}", normalized.trim_start_matches('\\')))
                };
                let allowed = [self.read.as_ref().filter(|_| access == Access::Read), self.write.as_ref()];
                if candidate.components().any(|c| c == Component::ParentDir)
                    || !allowed.iter().flatten().any(|r| {
                        // A special namespace needs a root in that namespace, not a disk root.
                        let disk_root = match r.resolved.components().next() {
                            Some(Component::Prefix(p)) => matches!(p.kind(), std::path::Prefix::VerbatimDisk(_)),
                            _ => false,
                        };
                        !disk_root && inside(&candidate, &r.resolved)
                    })
                {
                    return Err(format!("automation path rejected: network or device path outside the roots: {path}"));
                }
            }
        }
        if path.is_empty() {
            return Err("automation path rejected: path is empty".into());
        }
        // Check as given: making `…\NUL` absolute turns it into the device path `\\.\NUL`.
        if cfg!(windows) && Path::new(path).components().any(|c| matches!(c, Component::Normal(n) if is_device_name(&n.to_string_lossy()))) {
            return Err(format!("automation path rejected: reserved device names are not allowed: {path}"));
        }
        let absolute = std::path::absolute(path).map_err(|e| format!("automation path rejected: cannot resolve {path}: {e}"))?;
        #[cfg(windows)]
        if !is_disk_path(&absolute) {
            return Err(format!("automation path rejected: network or device path outside the roots: {path}"));
        }
        let target = resolve_path(&absolute)?;
        if access == Access::Read {
            let roots = [self.read.as_ref(), self.write.as_ref()];
            if roots.iter().flatten().any(|r| inside(&target, &r.resolved)) {
                return Ok(());
            }
            if roots.iter().all(|r| r.is_none()) {
                return Err(format!("automation filesystem access is not granted: read authority is absent: {path}"));
            }
            return Err(format!("automation path rejected: outside the read root (or the write root): {path}"));
        }
        let Some(Root { resolved: folder, shown }) = self.write.as_ref() else {
            return Err(format!("automation filesystem access is not granted: write authority is absent: {path}"));
        };
        if !inside(&target, folder) {
            return Err(format!("automation path rejected: outside the write root {}: {path}", shown.display()));
        }
        // A write to the root itself would put its `.cadcraft-tmp` file beside the root, outside it.
        if !directory && same(&target, folder) {
            return Err(format!("automation path rejected: the write root itself cannot be written: {path}"));
        }
        Ok(())
    }
}

thread_local! {
    /// The roots in force on this thread.
    static ROOTS: RefCell<Option<Arc<AutomationRoots>>> = const { RefCell::new(None) };
}

/// Puts the roots it replaced back when dropped (also when `f` panics).
struct Restore(Option<Arc<AutomationRoots>>);

impl Drop for Restore {
    fn drop(&mut self) {
        ROOTS.with(|r| *r.borrow_mut() = self.0.take());
    }
}

/// Run `f` with `roots` in force on this thread (`None`: as things stand).
pub fn confine<T>(roots: Option<&Arc<AutomationRoots>>, f: impl FnOnce() -> T) -> T {
    let previous = ROOTS.with(|r| r.replace(roots.cloned()));
    let _restore = Restore(previous);
    f()
}

/// This thread's roots, if any.
pub fn current() -> Option<Arc<AutomationRoots>> {
    ROOTS.with(|r| r.borrow().clone())
}

/// May `path` be read here? Always, without roots in force.
pub fn check_read(path: &str) -> Result<(), String> {
    current().map_or(Ok(()), |r| r.check(path, Access::Read))
}

/// May `path` be written here? Always, without roots in force.
pub fn check_write(path: &str) -> Result<(), String> {
    current().map_or(Ok(()), |r| r.check(path, Access::Write))
}

/// May files be stored in this directory? The write root itself is a valid directory.
pub fn check_write_directory(path: &str) -> Result<(), String> {
    current().map_or(Ok(()), |r| r.check_with(path, Access::Write, true, resolve))
}

/// `path` made absolute with its links followed; for a path that doesn't exist (yet), its nearest
/// existing folder's links followed and the rest appended.
fn resolve(absolute: &Path) -> Result<PathBuf, String> {
    let path = absolute.display();
    // The names below the nearest existing folder, last first.
    let mut missing: Vec<OsString> = vec![];
    let mut at = absolute;
    loop {
        #[cfg(test)]
        tests::before_filesystem_resolution();
        match std::fs::canonicalize(at) {
            Ok(mut found) => {
                found.extend(missing.iter().rev());
                return Ok(found);
            }
            Err(e) => match std::fs::symlink_metadata(at) {
                Ok(m) if m.file_type().is_symlink() => return Err(format!("automation path rejected: a link that leads nowhere: {path}")),
                // There, yet its real path can't be had (no permission): nothing to compare.
                Ok(_) => return Err(format!("automation path rejected: cannot resolve {path}: {e}")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("automation path rejected: cannot resolve {path}: {e}")),
            },
        }
        let parent = at.parent();
        match (at.components().next_back(), parent) {
            (Some(Component::Normal(name)), Some(parent)) => {
                missing.push(name.to_os_string());
                at = parent;
            }
            (Some(Component::ParentDir), _) => {
                return Err(format!("automation path rejected: parent traversal below a folder that doesn't exist: {path}"));
            }
            // Down to the root (or drive) and it doesn't exist either.
            _ => {
                let e = std::io::Error::from(std::io::ErrorKind::NotFound);
                return Err(format!("automation path rejected: cannot resolve {path}: {e}"));
            }
        }
    }
}

/// Is `path` (resolved) `root` or inside it, component by component (without regard to case on
/// Windows, whose file systems ignore it)?
fn inside(path: &Path, root: &Path) -> bool {
    let mut path = path.components();
    root.components().all(|r| path.next().is_some_and(|p| same_component(p, r)))
}

/// Is `path` (resolved) exactly `root`, compared like [`inside`] does?
fn same(path: &Path, root: &Path) -> bool {
    path.components().count() == root.components().count() && path.components().zip(root.components()).all(|(p, r)| same_component(p, r))
}

fn same_component(a: Component, b: Component) -> bool {
    if a == b {
        return true;
    }
    if !cfg!(windows) {
        return false;
    }
    match (a.as_os_str().to_str(), b.as_os_str().to_str()) {
        (Some(a), Some(b)) => a.to_lowercase() == b.to_lowercase(),
        _ => false,
    }
}

/// A Windows device name (`CON`, `NUL`, `COM1`… also with an extension), which names no file.
fn is_device_name(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component).trim_end_matches(' ').to_ascii_uppercase();
    let numbered = |prefix: &str| stem.strip_prefix(prefix).is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$") || numbered("COM") || numbered("LPT")
}

#[cfg(windows)]
fn is_disk_path(path: &Path) -> bool {
    match path.components().next() {
        Some(Component::Prefix(p)) => matches!(p.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static RESOLUTION_CALLS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    }

    pub(super) fn before_filesystem_resolution() {
        RESOLUTION_CALLS.with(|calls| {
            if let Some(n) = calls.get() {
                calls.set(Some(n + 1));
                panic!("rejected path reached filesystem resolution");
            }
        });
    }

    #[cfg(windows)]
    fn without_filesystem_resolution(f: impl FnOnce()) {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                RESOLUTION_CALLS.with(|c| c.set(None));
            }
        }
        RESOLUTION_CALLS.with(|c| c.set(Some(0)));
        let _restore = Restore;
        f();
        RESOLUTION_CALLS.with(|c| assert_eq!(c.get(), Some(0)));
    }

    #[cfg(windows)]
    fn slash_device_paths() -> Vec<String> {
        ["//?/", r"\\?/", r"/\?\", r"\/?/"].into_iter().map(|prefix| format!("{prefix}C:/../UNC/cc-roots-test.invalid/share/x.dxf")).collect()
    }

    #[cfg(windows)]
    #[test]
    fn normalized_namespaces_are_rejected_before_resolution() {
        let (base, roots) = folders("normalized");
        for path in slash_device_paths() {
            for access in [Access::Read, Access::Write] {
                let e = roots.check_with(&path, access, false, |_| panic!("normalization must precede filesystem resolution")).unwrap_err();
                assert!(e.contains("network or device path outside the roots"), "{path}: {e}");
            }
        }
        // Explicit UNC roots must not turn a non-disk normalized result into an allowed path.
        // Construct the authority without opening a network root.
        let remote = PathBuf::from(r"\\?\UNC\cc-roots-test.invalid\share");
        let roots = AutomationRoots {
            read: Some(Root { shown: remote.clone(), resolved: remote.clone() }),
            write: Some(Root { shown: remote.clone(), resolved: remote }),
        };
        for path in [r"\\cc-roots-test.invalid\share\x.dxf", r"\\?\UNC\cc-roots-test.invalid\share\x.dxf"] {
            for access in [Access::Read, Access::Write] {
                let e = roots.check_with(path, access, false, |_| panic!("non-disk result reached filesystem resolution")).unwrap_err();
                assert!(e.contains("network or device path outside the roots"), "{e}");
            }
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn slash_namespaces_are_blocked_at_file_boundaries() {
        // File hooks are process-wide: isolate this test from other tests' stand-in writers.
        if std::env::var_os("CADCRAFT_TEST_FILE_BOUNDARIES_CHILD").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .env("CADCRAFT_TEST_FILE_BOUNDARIES_CHILD", "1")
                .args(["--exact", "file_access::tests::slash_namespaces_are_blocked_at_file_boundaries", "--nocapture"])
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let (base, roots) = folders("normalized-boundaries");
        crate::cmd::file::set_io(crate::cmd::file::IoHooks {
            read: |bytes, path| cadcraft_io::read(bytes, path).map_err(|e| e.to_string()),
            write: |drawing, path| cadcraft_io::write(drawing, path).map_err(|e| e.to_string()),
            plot: None,
        });
        confine(Some(&roots), || {
            without_filesystem_resolution(|| {
                let mut session = crate::Session::new();
                for path in slash_device_paths() {
                    for command in ["open", "saveas", "export", "recover", "xattach"] {
                        let e = session.execute(command, &serde_json::json!({"path": path})).unwrap_err().to_string();
                        assert!(e.contains("automation path rejected"), "{command}: {e}");
                    }
                    // The filesystem path is built inside the interpreter, not visible to an outer parameter filter.
                    let literal = serde_json::to_string(&path).unwrap();
                    for call in ["(findfile p)", "(open p \"r\")", "(open p \"w\")", "(open p \"a\")", "(load p nil)"] {
                        let code = format!("(progn (setq p (strcat {literal} \"\")) {call})");
                        let value = session.lisp_eval(&code, false, true, vec![]).unwrap().unwrap().unwrap();
                        assert!(matches!(value, crate::lisp::value::Value::Nil), "{code}");
                    }
                }
            })
        });
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn normalized_disk_paths_inside_roots_are_accepted() {
        let (base, roots) = folders("normalized-positive");
        for (folder, access) in [("in", Access::Read), ("out", Access::Write)] {
            let dir = base.join(folder).join("Кириллица и пробелы").join("long".repeat(40)).join("deep".repeat(30));
            std::fs::create_dir_all(std::fs::canonicalize(base.join(folder)).unwrap().join(dir.strip_prefix(base.join(folder)).unwrap())).unwrap();
            let ordinary = s(dir.join("x.dxf"));
            assert!(ordinary.len() > 260);
            for path in [ordinary.clone(), format!(r"\\?\{ordinary}"), format!("//?/{}", ordinary.replace('\\', "/"))] {
                assert_eq!(roots.check(&path, access), Ok(()), "{path}");
            }
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn resolver_receives_the_checked_absolute_spelling() {
        let (base, roots) = folders("resolver-spelling");
        let mut paths = vec!["Cargo.toml".to_string()];
        #[cfg(windows)]
        paths.push(format!("//?/{}", s(base.join("in/a.dxf")).replace('\\', "/")));
        for path in paths.drain(..) {
            let expected = std::path::absolute(&path).unwrap();
            assert_eq!(
                roots.check_with(&path, Access::Read, false, |actual| {
                    assert_eq!(Path::new(actual), expected, "resolver must use the path whose prefix was checked");
                    Ok(std::fs::canonicalize(base.join("in/a.dxf")).unwrap())
                }),
                Ok(())
            );
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn normalized_prefix_policy_allows_only_disks() {
        for path in [r"C:\x", r"\\?\C:\x"] {
            assert!(is_disk_path(Path::new(path)), "{path}");
        }
        for path in [r"\\host\share\x", r"\\?\UNC\host\share\x", r"\\.\C:\x", r"\\?\pipe\x", r"\x", "relative", ""] {
            assert!(!is_disk_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn recovery_listing_checks_the_directory() {
        let (base, roots) = folders("recovery-list");
        let path = base.join("outside/Drawing_1234-abc_1.sv$");
        std::fs::write(&path, "private").unwrap();
        assert_eq!(crate::autosave::recoverable(&base.join("outside"), "").len(), 1);
        confine(Some(&roots), || {
            assert!(crate::autosave::recovery_entries(&base.join("outside")).is_none());
            assert!(crate::autosave::recoverable(&base.join("outside"), "").is_empty());
        });
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn recovery_discard_checks_each_write() {
        let (base, roots) = folders("recovery-discard");
        let path = base.join("in/Drawing_1234-abc_1.sv$");
        std::fs::write(&path, "private").unwrap();
        confine(Some(&roots), || crate::autosave::discard(&s(path.clone())));
        assert!(path.exists(), "a readable recovery file is not necessarily writable");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn recovery_prune_checks_each_write() {
        let (base, roots) = folders("recovery-prune");
        let path = base.join("in/1234-abc.lock");
        std::fs::write(&path, "private").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH)).unwrap();
        confine(Some(&roots), || crate::autosave::prune_locks(&base.join("in")));
        assert!(path.exists(), "a stale lock in a read-only root must survive");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn recovery_prune_checks_the_directory() {
        let (base, roots) = folders("prune-directory");
        let path = base.join("outside/1234-abc.lock");
        std::fs::write(&path, "private").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
            .unwrap();
        confine(Some(&roots), || crate::autosave::prune_locks(&base.join("outside")));
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn recovery_lock_metadata_does_not_follow_outside_links() {
        let (base, roots) = folders("recovery-lock-metadata");
        std::fs::write(base.join("in/Drawing_1234-abc_1.sv$"), "drawing").unwrap();
        let target = base.join("outside/secret.dxf");
        let link = base.join("in/1234-abc.lock");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        confine(Some(&roots), || assert_eq!(crate::autosave::recoverable(&base.join("in"), "").len(), 1));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn recovery_command_reports_denied_directory_and_discard() {
        let (base, roots) = folders("recovery-command");
        let mut session = crate::Session::new();
        let previous = crate::autosave::savefilepath();
        crate::autosave::set_savefilepath(&s(base.join("outside")));
        confine(Some(&roots), || {
            let e = session.execute("drawingrecovery", &serde_json::json!({})).unwrap_err();
            assert!(e.to_string().contains("outside the read root"), "{e}");
        });
        let path = base.join("in").join("Drawing_1234-abc_1.sv$");
        std::fs::write(&path, "private").unwrap();
        crate::autosave::set_savefilepath(&s(base.join("in")));
        confine(Some(&roots), || {
            let e = session.execute("drawingrecovery", &serde_json::json!({"action":"discard", "path":s(path.clone())})).unwrap_err();
            assert!(e.to_string().contains("outside the write root"), "{e}");
        });
        assert!(path.exists());
        crate::autosave::set_savefilepath(&previous);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn atomic_destination_checks_write_authority() {
        let (base, roots) = folders("atomic-destination");
        let target = base.join("outside/secret.dxf");
        let link = base.join("out/link.dxf");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        confine(Some(&roots), || assert!(crate::atomic_write::write_atomic(&link, b"escape").is_err()));
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn atomic_temporary_stays_inside_the_write_root() {
        let (base, roots) = folders("atomic-parent");
        let target = base.join("out/a.dxf");
        std::fs::write(&target, "original").unwrap();
        let link = base.join("outside/link.dxf");
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        confine(Some(&roots), || {
            assert!(crate::atomic_write::write_atomic(&link, b"escape").is_err());
        });
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn network_paths_are_rejected_before_resolution() {
        let (base, roots) = folders("network");
        for path in
            [r"\\127.0.0.1\c$\Windows\win.ini", r"\\?\UNC\127.0.0.1\c$\Windows\win.ini", r"\\.\C:\Windows\win.ini", "//127.0.0.1/c$/Windows/win.ini"]
        {
            for access in [Access::Read, Access::Write] {
                let e = roots.check_with(path, access, false, |_| panic!("rejected namespace reached filesystem resolution")).unwrap_err();
                assert!(e.contains("network or device path outside the roots"), "{e}");
            }
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn extended_devices_are_rejected_before_resolution() {
        let (base, roots) = folders("extended-devices");
        for path in [
            r"\\?\GLOBALROOT\Device\Mup\h\s\x",
            r"\\?\globalroot\Device\Mup\h\s\x",
            r"\\?\Global\UNC\h\s\x",
            r"\\?\gLoBaL\UNC\h\s\x",
            r"\\?\pipe\x",
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\x",
            r"\??\UNC\h\s\x",
            r"\??\C:\x",
        ] {
            for access in [Access::Read, Access::Write] {
                let e = roots.check_with(path, access, false, |_| panic!("rejected device reached filesystem resolution")).unwrap_err();
                assert!(e.contains("network or device path outside the roots"), "{e}");
            }
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_disk_paths_inside_roots_are_accepted() {
        let (base, roots) = folders("verbatim-disk");
        for (folder, access) in [("in", Access::Read), ("out", Access::Write)] {
            let path = std::fs::canonicalize(base.join(folder)).unwrap().join("new.dxf");
            assert!(path.to_string_lossy().starts_with(r"\\?\"));
            let calls = std::cell::Cell::new(0);
            assert_eq!(
                roots.check_with(&s(path.clone()), access, false, |_| {
                    calls.set(calls.get() + 1);
                    Ok(path.clone())
                }),
                Ok(())
            );
            assert_eq!(calls.get(), 1);
            assert_eq!(roots.check(&s(path), access), Ok(()));
        }
        let _ = std::fs::remove_dir_all(base);
    }

    /// A fresh folder holding `in` (read root), `out` (write root) and `outside`.
    fn folders(name: &str) -> (PathBuf, Arc<AutomationRoots>) {
        let base = std::env::temp_dir().join(format!("cc-file-access-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["in/sub", "out", "outside", "in2"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        std::fs::write(base.join("in/a.dxf"), "0\nEOF\n").unwrap();
        std::fs::write(base.join("outside/secret.dxf"), "0\nEOF\n").unwrap();
        let roots = AutomationRoots::new(Some(&base.join("in")), Some(&base.join("out"))).unwrap().unwrap();
        (base, roots)
    }

    /// `p` as a string a command would pass.
    fn s(p: PathBuf) -> String {
        p.to_string_lossy().into_owned()
    }

    /// Paths under either root may be read; writes stay inside the write root.
    #[test]
    fn paths_inside_the_roots_pass_and_others_dont() {
        let (base, roots) = folders("inside");
        assert_eq!(roots.check(&s(base.join("in/a.dxf")), Access::Read), Ok(()));
        assert_eq!(roots.check(&s(base.join("in")), Access::Read), Ok(()), "the root itself");
        assert_eq!(roots.check(&s(base.join("in/sub/new.dxf")), Access::Read), Ok(()), "missing files are checked by their folder");
        assert_eq!(roots.check(&s(base.join("out/new/deeper/x.png")), Access::Write), Ok(()), "and by their nearest folder");
        let e = roots.check(&s(base.join("outside/secret.dxf")), Access::Read).unwrap_err();
        assert!(e.starts_with("automation path rejected: outside the read root"), "{e}");
        // Reads are allowed under the write root too; writes are not allowed under the read root.
        assert_eq!(roots.check(&s(base.join("out/x.png")), Access::Read), Ok(()), "the write root also grants reads");
        let e = roots.check(&s(base.join("in/a.dxf")), Access::Write).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
        let _ = std::fs::remove_dir_all(base);
    }

    /// A write-only root still permits reads inside it; reads outside both roots are refused.
    #[test]
    fn reads_are_allowed_under_either_root() {
        let (base, _) = folders("either");
        let write_only = AutomationRoots::new(None, Some(&base.join("out"))).unwrap().unwrap();
        assert_eq!(write_only.check(&s(base.join("out/new.dxf")), Access::Read), Ok(()), "a file to read back");
        let e = write_only.check(&s(base.join("in/a.dxf")), Access::Read).unwrap_err();
        assert!(e.contains("outside the read root (or the write root)"), "{e}");
        let read_only = AutomationRoots::new(Some(&base.join("in")), None).unwrap().unwrap();
        assert_eq!(read_only.check(&s(base.join("in/a.dxf")), Access::Read), Ok(()));
        assert!(read_only.check(&s(base.join("out/new.dxf")), Access::Read).is_err(), "a read root alone grants no read of another folder");
        let _ = std::fs::remove_dir_all(base);
    }

    /// A write to the write root itself would put its `.cadcraft-tmp` file beside the root: refused.
    #[test]
    fn writing_to_the_write_root_itself_is_refused() {
        let (base, roots) = folders("write-root");
        let out = s(base.join("out"));
        let sep = std::path::MAIN_SEPARATOR;
        for p in [out.clone(), format!("{out}{sep}."), format!("{out}{sep}..{sep}out")] {
            let e = roots.check(&p, Access::Write).unwrap_err();
            assert!(e.contains("the write root itself cannot be written"), "{p}: {e}");
        }
        assert_eq!(roots.check(&out, Access::Read), Ok(()), "reading the root stays allowed");
        assert_eq!(roots.check(&s(base.join("out/new/x.png")), Access::Write), Ok(()), "a normal save inside still works");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_sibling_with_the_same_prefix_is_outside() {
        let (base, roots) = folders("prefix");
        std::fs::write(base.join("in2/b.dxf"), "0\nEOF\n").unwrap();
        assert!(roots.check(&s(base.join("in2/b.dxf")), Access::Read).is_err(), "in2 is not inside in");
        assert!(roots.check(&format!("{}2/c.dxf", s(base.join("in"))), Access::Read).is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn parent_traversal_is_resolved_or_refused() {
        let (base, roots) = folders("dotdot");
        let sep = std::path::MAIN_SEPARATOR;
        let up = format!("{}{sep}..{sep}outside{sep}secret.dxf", s(base.join("in")));
        assert!(roots.check(&up, Access::Read).unwrap_err().contains("outside the read root"), "`..` out of the root");
        let back = format!("{}{sep}..{sep}a.dxf", s(base.join("in/sub")));
        assert_eq!(roots.check(&back, Access::Read), Ok(()), "`..` that stays inside");
        let missing = format!("{}{sep}nope{sep}..{sep}..{sep}outside{sep}x.dxf", s(base.join("out")));
        let e = roots.check(&missing, Access::Write).unwrap_err();
        // Windows resolves `..` by name first (so does opening the file there): then it's outside.
        assert!(e.contains("parent traversal below a folder that doesn't exist") || e.contains("outside the write root"), "{e}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn relative_paths_resolve_against_the_working_directory() {
        let (base, roots) = folders("relative");
        // The tests' working directory is the crate's folder: not inside the roots.
        assert!(roots.check("a.dxf", Access::Read).unwrap_err().contains("outside the read root"));
        assert!(roots.check("", Access::Read).unwrap_err().contains("path is empty"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_missing_root_is_an_error_and_none_is_no_confinement() {
        let (base, _) = folders("missing");
        let e = AutomationRoots::new(Some(&base.join("nope")), None).unwrap_err();
        assert!(e.starts_with("cannot open the automation read root"), "{e}");
        let e = AutomationRoots::new(None, Some(&base.join("in/a.dxf"))).unwrap_err();
        assert!(
            e.starts_with("cannot open the automation write root")
                && e.ends_with(&std::io::Error::from(std::io::ErrorKind::NotADirectory).to_string()),
            "{e}"
        );
        assert!(AutomationRoots::new(Some(Path::new("")), None).unwrap_err().starts_with("cannot open the automation read root ``"));
        assert!(AutomationRoots::new(None, None).unwrap().is_none());
        // One root alone grants only its own access.
        let read_only = AutomationRoots::new(Some(&base.join("in")), None).unwrap().unwrap();
        let e = read_only.check(&s(base.join("in/x.dxf")), Access::Write).unwrap_err();
        assert!(e.contains("write authority is absent"), "{e}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn links_are_followed_before_comparing() {
        let (base, roots) = folders("links");
        #[cfg(unix)]
        let made = |target: &Path, link: &Path| std::os::unix::fs::symlink(target, link).is_ok();
        #[cfg(windows)]
        let made = |target: &Path, link: &Path| {
            let made =
                if target.is_dir() { std::os::windows::fs::symlink_dir(target, link) } else { std::os::windows::fs::symlink_file(target, link) };
            made.is_ok()
        };
        #[cfg(not(any(unix, windows)))]
        let made = |_: &Path, _: &Path| false;
        // Windows needs Developer Mode or elevation to make links: nothing to test without them.
        if !made(&base.join("outside/secret.dxf"), &base.join("in/leak.dxf")) {
            eprintln!("skipped: symbolic links can't be made here");
            let _ = std::fs::remove_dir_all(base);
            return;
        }
        assert!(roots.check(&s(base.join("in/leak.dxf")), Access::Read).unwrap_err().contains("outside the read root"), "a file link out");
        assert!(made(&base.join("outside"), &base.join("out/away")));
        let e = roots.check(&s(base.join("out/away/new.png")), Access::Write).unwrap_err();
        assert!(e.contains("outside the write root"), "a folder link out: {e}");
        assert!(made(&base.join("outside/gone.dxf"), &base.join("out/dangling.png")));
        let e = roots.check(&s(base.join("out/dangling.png")), Access::Write).unwrap_err();
        assert!(e.contains("a link that leads nowhere"), "writing would create the file outside: {e}");
        // A link to a folder inside the root stays inside.
        assert!(made(&base.join("in/sub"), &base.join("in/alias")));
        assert_eq!(roots.check(&s(base.join("in/alias/x.dxf")), Access::Read), Ok(()));
        // A root given through a link is the folder it leads to.
        assert!(made(&base.join("in"), &base.join("in-link")));
        let linked = AutomationRoots::new(Some(&base.join("in-link")), None).unwrap().unwrap();
        assert_eq!(linked.check(&s(base.join("in/a.dxf")), Access::Read), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(windows)]
    #[test]
    fn windows_ignores_case_and_refuses_devices() {
        let (base, roots) = folders("case");
        let upper = s(base.join("IN/A.DXF"));
        assert_eq!(roots.check(&upper, Access::Read), Ok(()), "{upper}");
        assert_eq!(roots.check(&s(base.join("In/Sub/New.dxf")), Access::Read), Ok(()));
        let e = roots.check(&s(base.join("out/NUL")), Access::Write).unwrap_err();
        assert!(e.contains("reserved device names"), "{e}");
        assert!(roots.check(&s(base.join("out/com1.txt")), Access::Write).is_err());
        assert_eq!(roots.check(&s(base.join("out/console.txt")), Access::Write), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn scopes_nest_and_restore_after_a_panic() {
        let (base, roots) = folders("scopes");
        let outside = s(base.join("outside/secret.dxf"));
        assert_eq!(check_read(&outside), Ok(()), "nothing is confined by default");
        confine(Some(&roots), || {
            assert!(check_read(&outside).is_err());
            // A nested scope replaces the outer one and gives it back on the way out.
            confine(None, || assert_eq!(check_read(&outside), Ok(())));
            assert!(check_read(&outside).is_err(), "restored after the inner scope");
        });
        assert_eq!(check_read(&outside), Ok(()), "restored after confine");
        let r = std::panic::catch_unwind(|| confine(Some(&roots), || panic!("boom")));
        assert!(r.is_err());
        assert_eq!(check_read(&outside), Ok(()), "restored after a panic");
        assert_eq!(confine(None, || check_read(&outside)), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn device_names() {
        for d in ["CON", "nul", "Com1", "LPT9.txt", "aux.svg", "NUL "] {
            assert!(is_device_name(d), "{d}");
        }
        for f in ["console", "com10", "COM0", "lpt", "a.svg", "nul-x"] {
            assert!(!is_device_name(f), "{f}");
        }
    }
}
