#![allow(
    unsafe_code,
    reason = "POSIX shared-memory input tests need a real named mmap fixture; pointers and descriptors stay inside this owner."
)]

use anyhow::{Context, Result};
use base64::engine::general_purpose;
use std::{
    ffi::CString,
    os::fd::{AsRawFd, FromRawFd},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT_SHARED_MEMORY_FIXTURE: AtomicUsize = AtomicUsize::new(0);

pub struct SharedMemoryFixture {
    name: CString,
}

impl SharedMemoryFixture {
    pub(crate) fn write(bytes: &[u8]) -> Result<Self> {
        let sequence = NEXT_SHARED_MEMORY_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!("/bt{:x}{sequence:x}", std::process::id()))?;
        // SAFETY: name is NUL terminated and remains live for this call. O_EXCL gives this
        // fixture exclusive ownership of the new name without unlinking someone else's object.
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("create shared-memory fixture");
        }

        let fixture = Self { name };
        // SAFETY: shm_open returned this newly owned descriptor and no other File owns it.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.set_len(u64::try_from(bytes.len())?)
            .context("size shared-memory fixture")?;
        if bytes.is_empty() {
            return Ok(fixture);
        }
        // SAFETY: file owns a writable object sized to bytes.len(); a null address lets the
        // kernel choose a fresh mapping. No pointer is used until MAP_FAILED is rejected.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes.len(),
                libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error()).context("map shared-memory fixture");
        }
        // SAFETY: the fresh mapping is writable for bytes.len() bytes and cannot overlap the
        // Rust slice. It is unmapped exactly once after copying, with no surviving reference.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapping.cast::<u8>(), bytes.len());
            if libc::munmap(mapping, bytes.len()) != 0 {
                return Err(std::io::Error::last_os_error()).context("unmap shared-memory fixture");
            }
        }
        Ok(fixture)
    }

    pub(crate) fn payload(&self) -> Result<String> {
        Ok(base64::Engine::encode(
            &general_purpose::STANDARD,
            self.name
                .to_str()
                .context("shared-memory name is not UTF-8")?
                .as_bytes(),
        ))
    }
}

impl Drop for SharedMemoryFixture {
    fn drop(&mut self) {
        // SAFETY: self owns this name after successful O_EXCL creation, including error paths.
        // Kitty may already have unlinked it; ignoring ENOENT is intentional.
        unsafe {
            libc::shm_unlink(self.name.as_ptr());
        }
    }
}

pub fn is_shared_memory_unavailable(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>().is_some_and(|io| {
        matches!(
            io.raw_os_error(),
            Some(code) if code == libc::ENXIO || code == libc::ENOSYS || code == libc::ENODEV
        )
    })
}
