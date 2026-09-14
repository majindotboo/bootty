# Transfers and forwards

Transfers and forwards are available through the command API. Local bindings copy
between local paths; SSH and WSL bindings stream through the versioned Bootty daemon.
SFTP-only accounts cannot use this command-channel transport.

Transfers use 64 KiB buffers, report bytes/total and phases, and support files up
to 1 TiB. Destination files are privately staged beside the final path, synced,
and published without replacing an existing file. SSH transfers verify byte
count and SHA-256 before publication. Source size/modification changes fail the
transfer. File modes/timestamps are not copied; new files use private staging
permissions. Parent directories must exist.

Cancel stops at an I/O boundary and terminates an owned SSH/WSL transport.
Incomplete staging is removed, including when a remote upload loses its input
channel. Cancellation can race a completed publication; a received completion
receipt remains authoritative. If a connection is lost after publication but
before its receipt, the error explicitly says completion is unconfirmed. Inspect
the destination before retrying. Retry keeps the original host and paths, creates
a fresh job, and never overwrites an existing destination or silently resumes a
partial byte range.

The window's existing job registry owns transfers, deadlines and cancellation.
Closing the window cancels transfers.
Jobs and transfers together share the eight-active / 64-record bounds and a
512 KiB summary-metadata limit. Completed records can be forgotten. This history
is not a persistent queue across app restarts.

## SSH forwards

The command API lists binding-owned forwards created by terminal links and
`forwards.open`. They expose remote loopback HTTP/HTTPS resources on private
local loopback listeners. Open checks the lease first; Check reports an observed
health result; Close waits for OpenSSH to acknowledge termination. Retry
establishes a replacement on the original host before retiring the old lease,
which can produce a new local port. A failed retry leaves the old lease intact.

These operations preserve configured OpenSSH arguments and authentication.
They require OpenSSH control-master support, currently Unix hosts. Windows WSL
localhost forwarding is owned by Windows, not represented as a Bootty SSH lease.
Retiring a binding or closing its window cleans up its forwards.

## Commands

- `transfers.start SPEC` targets a binding. JSON fields: `direction` (`upload`
  or `download`), `local_path`, `host_path`, `timeout_seconds` (1–86400; default
  3600). The response is a job handle with transfer metadata.
- `transfers.retry JOB_ID` creates a new job against the original captured host.
- `jobs.list`, `jobs.read JOB_ID CURSOR WAIT_MS`, `jobs.cancel JOB_ID`, and
  `jobs.forget JOB_ID` expose shared progress, lifetime and cleanup. Transfers
  publish `jobs.changed` events; their final success is `exited` with code zero.
- `forwards.list`, `forwards.open URL`, `forwards.check ID`, `forwards.retry ID`,
  and `forwards.close ID` use the shared invocation path.
  Open targets a binding; the remaining operations target the owning window.

The daemon's dedicated binary transfer stream contains bounded metadata, an
exact byte count, a checksum trailer and a final receipt. Large files never pass
through the bounded document editor or JSON command-output buffers.
