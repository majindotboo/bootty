# Terminal image clipboard writes

`session.clipboard-write-hosts` is a space-separated allow-list, empty by default.
Keys are `local`, `ssh:user@host:port` (omit `user@` when not configured), or
`wsl:distribution`. The SSH host is the configured alias, not a DNS-resolved name;
an unspecified port uses 22 in this key. Trust applies to every program running
in that host's terminal. Settings apply before accepting a write and again before
publishing it. OSC 52 text policy is unchanged.

OSC 5522 accepts one PNG, JPEG, GIF or WebP per write, at most 16 MiB encoded,
with 4096-byte decoded chunks and 8192-byte packets. Images decode off the UI
thread, with 8192-pixel dimensions, 16 megapixels and 64 MiB decoder allocation
limits. The native clipboard receives RGBA pixels (the first frame of animated
formats), not animation or original compressed bytes. MIME and actual encoding
must agree. Primary selection, reading and MIME aliases return `ENOSYS`.

One transfer or decode owns a window's image clipboard slot. Competing panes get
`EBUSY`; malformed transfers abort until a new write starts. Incomplete transfers
expire after 30 seconds. Permission denial returns `EPERM`, invalid data `EINVAL`,
platform failure `EIO`, and successful publication `DONE`. Replies retain the
client ID and route to the captured pane and binding generation. Closing the pane,
replacing the binding, or revoking its permission prevents a pending publication.
Opaque attachments without an addressable reply runtime do not support this path.

Packets travel as bounded terminal side effects. Replay drops them and clears
partial framing state; archive viewing never feeds them back into a terminal.
This protocol is separate from pasting a local screenshot into a remote host.

Protocol: <https://sw.kovidgoyal.net/kitty/clipboard/>. The implementation follows
its write/chunk/result contract; it does not advertise full read/alias support.
