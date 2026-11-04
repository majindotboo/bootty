//! Every Win32 call Bootty makes for IPC identity, process trees and detached spawning.
//!
//! Deliberate limit: this is the only module that allows `unsafe`. Job objects, pipe security
//! descriptors and token queries have no safe wrapper in the dependency tree; move a call out
//! when one appears.
#![expect(
    unsafe_code,
    reason = "Win32 job objects, pipe security and token queries have no safe wrapper"
)]

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command, ExitStatus};
use std::ptr::{null, null_mut};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::Foundation::{
    ERROR_INVALID_HANDLE, ERROR_PIPE_BUSY, GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE, LocalFree, SetHandleInformation,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    DETACHED_PROCESS, GetCurrentProcess, OpenProcess, OpenProcessToken, OpenThread,
    PROCESS_QUERY_LIMITED_INFORMATION, ResumeThread, THREAD_SUSPEND_RESUME,
};

use crate::identity::UserIdentity;
use crate::ipc::PeerIdentity;
use crate::process_tree::ConsoleWindowBehavior;
use crate::spawn::CommandSpawn as _;

/// The SID of the account that owns this process.
pub fn current_user_sid() -> io::Result<String> {
    let mut token = null_mut();
    // SAFETY: The current-process pseudo handle needs no close; `token` is a writable out slot.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    token_user_sid(&owned(token)?)
}

fn process_user_sid(pid: u32) -> io::Result<String> {
    // SAFETY: Opening a process by ID borrows nothing; the result is checked by `owned`.
    let process = owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })?;
    let mut token = null_mut();
    // SAFETY: `process` is live for the call and `token` is a writable out slot.
    let ok = unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &raw mut token) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    token_user_sid(&owned(token)?)
}

fn token_user_sid(token: &OwnedHandle) -> io::Result<String> {
    let mut needed = 0_u32;
    // SAFETY: A null buffer with zero length only reports the required size.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            null_mut(),
            0,
            &raw mut needed,
        )
    };
    let words = usize::try_from(needed)
        .map_err(io::Error::other)?
        .div_ceil(size_of::<usize>());
    // Pointer-aligned storage, as TOKEN_USER requires.
    let mut buffer = vec![0_usize; words.max(1)];
    let length =
        u32::try_from(buffer.len().saturating_mul(size_of::<usize>())).map_err(io::Error::other)?;
    // SAFETY: `buffer` is writable for `length` bytes for the duration of the call.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &raw mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: The call wrote a TOKEN_USER at the start of the aligned buffer; its SID points into
    // the same buffer, which outlives the conversion below.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_string(sid)
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut wide = null_mut();
    // SAFETY: `sid` is a valid SID for the call and `wide` is a writable out slot.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut wide) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: On success `wide` is a NUL-terminated UTF-16 string from LocalAlloc, read up to its
    // terminator and freed exactly once.
    let text = unsafe {
        let length = (0..usize::MAX)
            .take_while(|&index| *wide.add(index) != 0)
            .count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(wide, length));
        LocalFree(wide.cast());
        text
    };
    Ok(text)
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: A valid handle a Win32 call just returned to us, owned nowhere else.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn is_os_error(error: &io::Error, code: u32) -> bool {
    error.raw_os_error().and_then(|raw| u32::try_from(raw).ok()) == Some(code)
}

fn wide_null(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain([0]).collect()
}

/// An owned security descriptor parsed from SDDL.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let sddl = wide_null(sddl);
        let mut descriptor = null_mut();
        // SAFETY: `sddl` is NUL-terminated UTF-16 and `descriptor` is a writable out slot that
        // receives a LocalAlloc block this value frees.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: The descriptor came from LocalAlloc and is freed exactly once.
        unsafe { LocalFree(self.0) };
    }
}

/// One pipe instance only this account can open, rejecting remote clients.
pub fn create_pipe_server(name: &OsStr, first: bool) -> io::Result<NamedPipeServer> {
    let sid = current_user_sid()?;
    let descriptor = SecurityDescriptor::from_sddl(&format!("O:{sid}G:{sid}D:P(A;;GA;;;{sid})"))?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).map_err(io::Error::other)?,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true);
    // SAFETY: `attributes` and the descriptor it points at outlive CreateNamedPipeW inside Tokio.
    unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) }
}

pub fn pipe_client_identity(server: &NamedPipeServer) -> io::Result<PeerIdentity> {
    let mut pid = 0_u32;
    // SAFETY: `server` is a connected pipe handle and `pid` is a writable out slot.
    if unsafe { GetNamedPipeClientProcessId(server.as_raw_handle(), &raw mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let user = UserIdentity::from_sid(process_user_sid(pid)?);
    Ok(PeerIdentity { pid, user })
}

/// A blocking named-pipe client: a Tokio client driven by its own current-thread runtime.
pub struct BlockingLocalStream {
    runtime: tokio::runtime::Runtime,
    client: NamedPipeClient,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl std::fmt::Debug for BlockingLocalStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BlockingLocalStream")
    }
}

impl BlockingLocalStream {
    /// Transfers the runtime and pipe to an independently driven adapter.
    #[must_use]
    pub fn into_parts(self) -> (tokio::runtime::Runtime, NamedPipeClient) {
        (self.runtime, self.client)
    }

    /// # Errors
    /// Never fails; the signature matches `UnixStream`.
    pub const fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout = timeout;
        Ok(())
    }

    /// # Errors
    /// Never fails; the signature matches `UnixStream`.
    pub const fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.write_timeout = timeout;
        Ok(())
    }
}

fn block_on<T>(
    runtime: &tokio::runtime::Runtime,
    timeout: Option<Duration>,
    operation: impl Future<Output = io::Result<T>>,
) -> io::Result<T> {
    match timeout {
        Some(timeout) => runtime.block_on(async {
            tokio::time::timeout(timeout, operation)
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "named pipe timed out"))?
        }),
        None => runtime.block_on(operation),
    }
}

impl Read for BlockingLocalStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        block_on(&self.runtime, self.read_timeout, self.client.read(buffer))
    }
}

impl Write for BlockingLocalStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        block_on(&self.runtime, self.write_timeout, self.client.write(buffer))
    }

    fn flush(&mut self) -> io::Result<()> {
        block_on(&self.runtime, self.write_timeout, self.client.flush())
    }
}

/// Opens `name`, retrying busy instances until `timeout`, and refuses a server owned by another
/// account so a squatted pipe never receives a request.
pub fn connect_pipe(name: &OsStr, timeout: Duration) -> io::Result<BlockingLocalStream> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    let deadline = Instant::now().checked_add(timeout);
    let client = loop {
        let opened = {
            let _context = runtime.enter();
            ClientOptions::new().open(name)
        };
        match opened {
            Ok(client) => break client,
            Err(error)
                if is_os_error(&error, ERROR_PIPE_BUSY)
                    && deadline.is_some_and(|deadline| Instant::now() < deadline) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    };
    let mut server_pid = 0_u32;
    // SAFETY: `client` is a connected pipe handle and `server_pid` is a writable out slot.
    if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &raw mut server_pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if process_user_sid(server_pid)? != current_user_sid()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the named pipe belongs to another account",
        ));
    }
    Ok(BlockingLocalStream {
        runtime,
        client,
        read_timeout: None,
        write_timeout: None,
    })
}

/// A kill-on-close job holding one child's whole process tree.
pub struct ProcessJob {
    handle: OwnedHandle,
}

impl ProcessJob {
    /// Starts the child suspended, assigns it to a fresh job, then resumes it, so no descendant
    /// can start outside the job.
    pub fn spawn(
        command: &mut Command,
        console_window: ConsoleWindowBehavior,
    ) -> io::Result<(Child, Self)> {
        let console = match console_window {
            ConsoleWindowBehavior::Inherit => 0,
            ConsoleWindowBehavior::Suppress => CREATE_NO_WINDOW,
        };
        command.creation_flags(CREATE_SUSPENDED | console);
        let mut child = command.bootty_spawn()?;
        let job = match Self::for_child(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        if let Err(error) = resume_primary_thread(child.id()) {
            let _ = job.terminate();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok((child, job))
    }

    fn for_child(child: &Child) -> io::Result<Self> {
        // SAFETY: Null attributes and name request an unnamed job; `owned` checks the result.
        let handle = owned(unsafe { CreateJobObjectW(null(), null()) })?;
        let job = Self { handle };
        job.set_limit_flags(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)?;
        // SAFETY: Both handles are live for the call, which takes ownership of neither.
        let ok =
            unsafe { AssignProcessToJobObject(job.handle.as_raw_handle(), child.as_raw_handle()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn set_limit_flags(&self, flags: u32) -> io::Result<()> {
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = flags;
        let length = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .map_err(io::Error::other)?;
        // SAFETY: `limits` is initialized, of the declared size, and borrowed only for the call.
        let ok = unsafe {
            SetInformationJobObject(
                self.handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                length,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: The job handle is live and owned by `self`.
        if unsafe { TerminateJobObject(self.handle.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[expect(
        clippy::unused_self,
        reason = "matches the Unix process-group interface"
    )]
    pub fn has_exited(&self, child: &mut Child) -> io::Result<bool> {
        child.try_wait().map(|status| status.is_some())
    }

    /// Waits for the child, then lets background descendants it left outlive the job handle.
    pub fn wait(&self, child: &mut Child) -> io::Result<ExitStatus> {
        let status = child.wait()?;
        self.set_limit_flags(0)?;
        Ok(status)
    }

    #[expect(
        clippy::unused_self,
        reason = "matches the Unix process-group interface"
    )]
    pub const fn disarm(&self) {}
}

fn resume_primary_thread(pid: u32) -> io::Result<()> {
    const ATTEMPTS: usize = 20;
    for _ in 0..ATTEMPTS {
        if let Some(thread) = primary_thread(pid)? {
            // SAFETY: The ID came from a live snapshot; `owned` checks the returned handle.
            let thread = owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread) })?;
            // SAFETY: `thread` is live and opened with THREAD_SUSPEND_RESUME.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "the suspended child's primary thread was not found",
    ))
}

fn primary_thread(pid: u32) -> io::Result<Option<u32>> {
    // SAFETY: A system-wide thread snapshot borrows nothing; `owned` checks the result.
    let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
    let mut entry = THREADENTRY32 {
        dwSize: u32::try_from(size_of::<THREADENTRY32>()).map_err(io::Error::other)?,
        ..THREADENTRY32::default()
    };
    // SAFETY: `snapshot` is live and `entry` is writable with its size field set.
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    while found {
        if entry.th32OwnerProcessID == pid {
            return Ok(Some(entry.th32ThreadID));
        }
        // SAFETY: The same live snapshot and writable entry.
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    }
    Ok(None)
}

/// Spawns without a console, outside the launcher's job, and with the launcher's own standard
/// handles made uninheritable for the duration of the spawn.
pub fn spawn_hidden(command: &mut Command) -> io::Result<Child> {
    command.creation_flags(
        DETACHED_PROCESS
            | CREATE_NO_WINDOW
            | CREATE_UNICODE_ENVIRONMENT
            | CREATE_BREAKAWAY_FROM_JOB,
    );
    let _guard = StdHandleInheritanceGuard::new()?;
    command.bootty_spawn()
}

/// Clears the inherit flag on this process's standard handles until dropped. One spawn at a time
/// holds it, so concurrent spawns never observe a half-restored set.
struct StdHandleInheritanceGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    cleared: Vec<HANDLE>,
}

impl StdHandleInheritanceGuard {
    fn new() -> io::Result<Self> {
        static LOCK: Mutex<()> = Mutex::new(());
        let mut guard = Self {
            _lock: LOCK.lock().unwrap_or_else(PoisonError::into_inner),
            cleared: Vec::new(),
        };
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: Reading a standard handle borrows nothing.
            let handle = unsafe { GetStdHandle(which) };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                continue;
            }
            let mut flags = 0_u32;
            // SAFETY: `handle` is this process's standard handle; `flags` is a writable out slot.
            if unsafe { GetHandleInformation(handle, &raw mut flags) } == 0 {
                let error = io::Error::last_os_error();
                if is_os_error(&error, ERROR_INVALID_HANDLE) {
                    continue;
                }
                return Err(error);
            }
            if flags & HANDLE_FLAG_INHERIT != 0 {
                // SAFETY: Changes only the inherit flag of a live standard handle.
                if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                guard.cleared.push(handle);
            }
        }
        Ok(guard)
    }
}

impl Drop for StdHandleInheritanceGuard {
    fn drop(&mut self) {
        for handle in self.cleared.drain(..) {
            // SAFETY: Restores the inherit flag this guard cleared on the same standard handle.
            let _ =
                unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
        }
    }
}
