use std::io;
use std::time::Duration;

use super::LocalEndpoint;
use crate::identity::UserIdentity;

/// The process and account on the other end of an accepted connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerIdentity {
    pub pid: u32,
    pub user: UserIdentity,
}

impl PeerIdentity {
    /// Whether the peer runs as this process's account.
    #[must_use]
    pub fn is_current_user(&self) -> bool {
        UserIdentity::current().is_ok_and(|user| user == self.user)
    }
}

/// An accepted server-side stream.
#[cfg(unix)]
pub type LocalStream = tokio::net::UnixStream;
#[cfg(windows)]
pub type LocalStream = tokio::net::windows::named_pipe::NamedPipeServer;

/// A blocking client stream with optional read and write timeouts.
#[cfg(unix)]
pub type BlockingLocalStream = std::os::unix::net::UnixStream;
#[cfg(windows)]
pub use crate::windows::BlockingLocalStream;

/// Connects to `endpoint`, waiting at most `timeout` for the listener to take the connection.
/// On Windows the server must run as this account.
///
/// # Errors
/// Returns a missing endpoint, a refused or timed-out connection, or a server owned by another
/// account.
pub fn connect_blocking(
    endpoint: &LocalEndpoint,
    timeout: Duration,
) -> io::Result<BlockingLocalStream> {
    #[cfg(unix)]
    {
        unix::connect(endpoint, timeout)
    }
    #[cfg(windows)]
    {
        crate::windows::connect_pipe(endpoint.as_path().as_os_str(), timeout)
    }
}

#[cfg(unix)]
mod unix {
    use std::io;
    use std::time::{Duration, Instant};

    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::io::Errno;
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketType, sockopt::socket_error};

    use super::{BlockingLocalStream, LocalEndpoint};

    /// A nonblocking connect bounded by `timeout`; a full listen backlog would otherwise block
    /// forever.
    pub(super) fn connect(
        endpoint: &LocalEndpoint,
        timeout: Duration,
    ) -> io::Result<BlockingLocalStream> {
        let address = SocketAddrUnix::new(endpoint.as_path())?;
        let socket = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None)?;
        rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC)?;
        rustix::io::ioctl_fionbio(&socket, true)?;
        match rustix::net::connect(&socket, &address) {
            Ok(()) => {}
            Err(Errno::INPROGRESS | Errno::WOULDBLOCK) => wait_writable(&socket, timeout)
                .and_then(|()| socket_error(&socket)?.map_err(io::Error::from))
                .map_err(|error| with_endpoint(&error, endpoint))?,
            Err(error) => return Err(io::Error::from(error)),
        }
        let stream = BlockingLocalStream::from(socket);
        stream.set_nonblocking(false)?;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let peer_uid = rustix::net::sockopt::socket_peercred(&stream)?.uid.as_raw();
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let peer_uid = nix::unistd::getpeereid(&stream)?.0.as_raw();
        if peer_uid != rustix::process::getuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "local IPC server belongs to another account",
            ));
        }
        Ok(stream)
    }

    fn wait_writable(socket: &rustix::fd::OwnedFd, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            let remaining = deadline.map_or(timeout, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("timed out after {}s connecting", timeout.as_secs_f32()),
                ));
            }
            let wait = Timespec {
                tv_sec: i64::try_from(remaining.as_secs()).unwrap_or(i64::MAX),
                tv_nsec: remaining.subsec_nanos().into(),
            };
            let mut fds = [PollFd::new(
                socket,
                PollFlags::OUT | PollFlags::ERR | PollFlags::HUP,
            )];
            match poll(&mut fds, Some(&wait)) {
                Ok(0) | Err(Errno::INTR) => {}
                Ok(_) => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn with_endpoint(error: &io::Error, endpoint: &LocalEndpoint) -> io::Error {
        io::Error::new(
            error.kind(),
            format!("{error} ({})", endpoint.as_path().display()),
        )
    }
}
