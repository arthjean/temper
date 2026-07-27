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

        let signal_mask = interrupt
            .map(|_| SigintMask::block())
            .transpose()
            .inspect_err(|_error| {
                let _removed = self.unlink(&temporary_name);
            })?;
        if interrupt.is_some_and(|check| check())
            || signal_mask.as_ref().is_some_and(SigintMask::sigint_pending)
        {
            drop(signal_mask);
            let _removed = self.unlink(&temporary_name);
            return Err(interrupted_before_commit());
        }
        if let Err(error) = self.rename(&temporary_name, name, 0) {
            drop(signal_mask);
            let _removed = self.unlink(&temporary_name);
            return Err(error);
        }
        if let Err(error) = self.file.sync_all() {
            drop(signal_mask);
            return Err(AnchoredError {
                message: format!(
                    "JSON rename committed, but its directory could not be synced: {error}"
                ),
                committed: true,
                interrupted: false,
            });
        }
        drop(signal_mask);
        Ok(())
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
