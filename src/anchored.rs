use std::ffi::CString;
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use serde::Serialize;

#[derive(Debug)]
pub(crate) struct AnchoredError {
    pub(crate) message: String,
    pub(crate) committed: bool,
    pub(crate) interrupted: bool,
}

pub(crate) struct AnchoredDirectory {
    file: File,
}

impl AnchoredDirectory {
    pub(crate) fn open(path: &Path) -> Result<Self, AnchoredError> {
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| before_commit("Directory path contained a NUL byte."))?;
        let descriptor = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        file_from_descriptor(descriptor, "Could not anchor directory").map(|file| Self { file })
    }

    pub(crate) fn create_child(&self, name: &str, mode: u32) -> Result<Self, AnchoredError> {
        let name = component(name)?;
        let created =
            unsafe { libc::mkdirat(self.file.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) };
        if created != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(before_commit(format!(
                    "Could not create anchored directory: {error}"
                )));
            }
        }
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        let file = file_from_descriptor(descriptor, "Could not open anchored directory")?;
        if unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) } != 0 {
            return Err(before_commit(format!(
                "Could not secure anchored directory: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self { file })
    }

    pub(crate) fn create_file(&self, name: &str, mode: u32) -> Result<File, AnchoredError> {
        let name = component(name)?;
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::mode_t,
            )
        };
        file_from_descriptor(descriptor, "Could not create anchored file")
    }

    pub(crate) fn rename_noreplace(
        &self,
        source: &str,
        destination: &str,
    ) -> Result<(), AnchoredError> {
        self.rename(source, destination, libc::RENAME_NOREPLACE)
    }

    pub(crate) fn replace_and_sync(
        &self,
        source: &str,
        destination: &str,
    ) -> Result<(), AnchoredError> {
        self.rename(source, destination, 0)?;
        self.file.sync_all().map_err(|error| AnchoredError {
            message: format!(
                "Anchored rename committed, but its directory could not be synced: {error}"
            ),
            committed: true,
            interrupted: false,
        })
    }

    pub(crate) fn unlink(&self, name: &str) -> Result<(), AnchoredError> {
        let name = component(name)?;
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } == 0 {
            Ok(())
        } else {
            Err(before_commit(format!(
                "Could not remove anchored file: {}",
                std::io::Error::last_os_error()
            )))
        }
    }

    pub(crate) fn sync(&self) -> Result<(), AnchoredError> {
        self.file
            .sync_all()
            .map_err(|error| before_commit(format!("Could not sync anchored directory: {error}")))
    }

    pub(crate) fn write_json_atomic(
        &self,
        name: &str,
        value: &impl Serialize,
        interrupt: Option<&dyn Fn() -> bool>,
    ) -> Result<(), AnchoredError> {
        self.write_json_atomic_inner(name, value, interrupt, false, &|| self.file.sync_all())
    }

    pub(crate) fn write_json_atomic_preserving_previous(
        &self,
        name: &str,
        value: &impl Serialize,
        interrupt: Option<&dyn Fn() -> bool>,
    ) -> Result<(), AnchoredError> {
        self.write_json_atomic_inner(name, value, interrupt, true, &|| self.file.sync_all())
    }

    pub(crate) fn reserve_space(&self, name: &str, bytes: u64) -> Result<(), AnchoredError> {
        let length = libc::off_t::try_from(bytes)
            .map_err(|_| before_commit("Reserved file length exceeded Linux off_t."))?;
        let file = self.create_file(name, 0o600)?;
        let allocation = unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, length) };
        if allocation != 0 {
            drop(file);
            let _removed = self.unlink(name);
            return Err(before_commit(format!(
                "Could not reserve filesystem space: {}",
                std::io::Error::from_raw_os_error(allocation)
            )));
        }
        if let Err(error) = file.sync_all() {
            drop(file);
            let _removed = self.unlink(name);
            return Err(before_commit(format!(
                "Could not sync reserved filesystem space: {error}"
            )));
        }
        if let Err(error) = self.sync() {
            drop(file);
            let _removed = self.unlink(name);
            let _synced_cleanup = self.sync();
            return Err(error);
        }
        Ok(())
    }

    fn write_json_atomic_inner(
        &self,
        name: &str,
        value: &impl Serialize,
        interrupt: Option<&dyn Fn() -> bool>,
        preserve_previous: bool,
        sync_directory: &dyn Fn() -> std::io::Result<()>,
    ) -> Result<(), AnchoredError> {
        if interrupt.is_some_and(|check| check()) {
            return Err(interrupted_before_commit());
        }
        let mut bytes = serde_json::to_vec_pretty(value)
            .map_err(|error| before_commit(format!("Could not serialize JSON: {error}")))?;
        bytes.push(b'\n');
        let temporary_name = format!(".{name}.tmp-{}", std::process::id());
        let mut temporary = self.create_file(&temporary_name, 0o600)?;
        let write_result = temporary
            .write_all(&bytes)
            .and_then(|()| temporary.sync_all());
        if let Err(error) = write_result {
            let _removed = self.unlink(&temporary_name);
            return Err(before_commit(format!(
                "Could not write and sync temporary JSON: {error}"
            )));
        }

        let backup_name = format!(".{name}.previous-{}", std::process::id());
        let previous_linked = if preserve_previous {
            let _removed_stale_backup = self.unlink(&backup_name);
            match self.link_existing(name, &backup_name) {
                Ok(linked) => linked,
                Err(error) => {
                    let _removed = self.unlink(&temporary_name);
                    return Err(error);
                }
            }
        } else {
            false
        };
        if previous_linked && let Err(error) = sync_directory() {
            let _removed_temporary = self.unlink(&temporary_name);
            let _removed_backup = self.unlink(&backup_name);
            return Err(before_commit(format!(
                "Could not make the previous JSON recovery link durable: {error}"
            )));
        }

        let signal_mask = interrupt
            .map(|_| SigintMask::block())
            .transpose()
            .inspect_err(|_error| {
                let _removed = self.unlink(&temporary_name);
                if previous_linked {
                    let _removed = self.unlink(&backup_name);
                }
            })?;
        if interrupt.is_some_and(|check| check())
            || signal_mask.as_ref().is_some_and(SigintMask::sigint_pending)
        {
            drop(signal_mask);
            let _removed = self.unlink(&temporary_name);
            if previous_linked {
                let _removed = self.unlink(&backup_name);
            }
            return Err(interrupted_before_commit());
        }
        if let Err(error) = self.rename(&temporary_name, name, 0) {
            drop(signal_mask);
            let _removed = self.unlink(&temporary_name);
            if previous_linked {
                let _removed = self.unlink(&backup_name);
            }
            return Err(error);
        }
        if let Err(error) = sync_directory() {
            let rollback = if previous_linked {
                self.rename(&backup_name, name, 0)
            } else {
                self.unlink(name)
            };
            let rollback = rollback.and_then(|()| {
                sync_directory().map_err(|rollback_error| {
                    before_commit(format!(
                        "Could not sync the restored JSON state: {rollback_error}"
                    ))
                })
            });
            drop(signal_mask);
            if rollback.is_ok() {
                return Err(before_commit(format!(
                    "JSON publication could not be made durable and the previous state was restored: {error}"
                )));
            }
            return Err(AnchoredError {
                message: format!(
                    "JSON rename committed, its directory could not be synced, and the previous state could not be restored durably: {error}"
                ),
                committed: true,
                interrupted: false,
            });
        }
        drop(signal_mask);
        if previous_linked && self.unlink(&backup_name).is_ok() {
            let _synced_cleanup = sync_directory();
        }
        Ok(())
    }

    fn link_existing(&self, source: &str, destination: &str) -> Result<bool, AnchoredError> {
        let source = component(source)?;
        let destination = component(destination)?;
        let linked = unsafe {
            libc::linkat(
                self.file.as_raw_fd(),
                source.as_ptr(),
                self.file.as_raw_fd(),
                destination.as_ptr(),
                0,
            )
        };
        if linked == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(false)
        } else {
            Err(before_commit(format!(
                "Could not preserve the previous anchored file: {error}"
            )))
        }
    }

    fn rename(
        &self,
        source: &str,
        destination: &str,
        flags: libc::c_uint,
    ) -> Result<(), AnchoredError> {
        let source = component(source)?;
        let destination = component(destination)?;
        let result = unsafe {
            libc::renameat2(
                self.file.as_raw_fd(),
                source.as_ptr(),
                self.file.as_raw_fd(),
                destination.as_ptr(),
                flags,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(before_commit(format!(
                "Could not rename anchored file: {}",
                std::io::Error::last_os_error()
            )))
        }
    }
}

fn component(name: &str) -> Result<CString, AnchoredError> {
    if name.is_empty() || name == "." || name == ".." || name.as_bytes().contains(&b'/') {
        return Err(before_commit("Anchored path must be one plain component."));
    }
    CString::new(name).map_err(|_| before_commit("Anchored path contained a NUL byte."))
}

fn file_from_descriptor(descriptor: RawFd, context: &str) -> Result<File, AnchoredError> {
    if descriptor < 0 {
        Err(before_commit(format!(
            "{context}: {}",
            std::io::Error::last_os_error()
        )))
    } else {
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn before_commit(message: impl Into<String>) -> AnchoredError {
    AnchoredError {
        message: message.into(),
        committed: false,
        interrupted: false,
    }
}

fn interrupted_before_commit() -> AnchoredError {
    AnchoredError {
        message: "Publication was interrupted before its atomic commit.".to_owned(),
        committed: false,
        interrupted: true,
    }
}

struct SigintMask {
    previous: libc::sigset_t,
}

impl SigintMask {
    fn block() -> Result<Self, AnchoredError> {
        let mut signal_set = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        let mut previous = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        if unsafe { libc::sigemptyset(&mut signal_set) } != 0
            || unsafe { libc::sigaddset(&mut signal_set, libc::SIGINT) } != 0
            || unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &signal_set, &mut previous) } != 0
        {
            return Err(before_commit(format!(
                "Could not block SIGINT around publication: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self { previous })
    }

    fn sigint_pending(&self) -> bool {
        let mut pending = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        (unsafe { libc::sigpending(&mut pending) }) == 0
            && (unsafe { libc::sigismember(&pending, libc::SIGINT) }) == 1
    }
}

impl Drop for SigintMask {
    fn drop(&mut self) {
        let _restored = unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut())
        };
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::io;

    use serde_json::json;

    use super::AnchoredDirectory;

    #[test]
    fn post_rename_sync_failure_restores_the_previous_json() {
        let root = tempfile::tempdir().expect("temporary anchored directory");
        let previous = b"{\"generation\":\"previous\"}\n";
        fs::write(root.path().join("latest.json"), previous).expect("seed latest");
        let directory = AnchoredDirectory::open(root.path()).expect("anchor directory");
        let sync_calls = Cell::new(0_u8);

        let error = directory
            .write_json_atomic_inner(
                "latest.json",
                &json!({"generation": "new"}),
                None,
                true,
                &|| {
                    let call = sync_calls.get();
                    sync_calls.set(call + 1);
                    if call == 1 {
                        Err(io::Error::other("injected post-rename sync failure"))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("injected sync failure");

        assert!(!error.committed);
        assert_eq!(
            fs::read(root.path().join("latest.json")).expect("restored latest"),
            previous
        );
        assert_eq!(sync_calls.get(), 3);
        assert!(
            fs::read_dir(root.path())
                .expect("anchored directory entries")
                .all(|entry| !entry
                    .expect("anchored entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".latest.json."))
        );
    }

    #[test]
    fn post_rename_sync_failure_removes_a_new_json_without_a_previous_value() {
        let root = tempfile::tempdir().expect("temporary anchored directory");
        let directory = AnchoredDirectory::open(root.path()).expect("anchor directory");
        let sync_calls = Cell::new(0_u8);

        let error = directory
            .write_json_atomic_inner(
                "latest.json",
                &json!({"generation": "new"}),
                None,
                true,
                &|| {
                    let call = sync_calls.get();
                    sync_calls.set(call + 1);
                    if call == 0 {
                        Err(io::Error::other("injected post-rename sync failure"))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("injected sync failure");

        assert!(!error.committed);
        assert!(!root.path().join("latest.json").exists());
        assert_eq!(sync_calls.get(), 2);
    }

    #[test]
    fn reserved_space_is_allocated_and_releasable() {
        let root = tempfile::tempdir().expect("temporary anchored directory");
        let directory = AnchoredDirectory::open(root.path()).expect("anchor directory");

        directory
            .reserve_space(".failure-reserve", 128 * 1024)
            .expect("reserve filesystem space");

        assert_eq!(
            fs::metadata(root.path().join(".failure-reserve"))
                .expect("reserved file metadata")
                .len(),
            128 * 1024
        );
        directory
            .unlink(".failure-reserve")
            .expect("release reserved space");
    }

    #[test]
    fn preallocated_emergency_file_replaces_json_without_a_content_write() {
        let root = tempfile::tempdir().expect("temporary anchored directory");
        fs::write(
            root.path().join("run.json"),
            b"{\"status\":\"promotion\"}\n",
        )
        .expect("seed active manifest");
        let emergency = b"{\"status\":\"failed\"}\n";
        fs::write(root.path().join(".promotion-failure.json"), emergency)
            .expect("seed emergency manifest");
        let directory = AnchoredDirectory::open(root.path()).expect("anchor directory");

        directory
            .replace_and_sync(".promotion-failure.json", "run.json")
            .expect("activate emergency manifest");

        assert_eq!(
            fs::read(root.path().join("run.json")).expect("read replacement manifest"),
            emergency
        );
        assert!(!root.path().join(".promotion-failure.json").exists());
    }
}
