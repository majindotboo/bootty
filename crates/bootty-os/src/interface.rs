//! Local network interfaces.

use std::net::Ipv6Addr;

/// The index of the local interface that holds `address`, or `None` when none holds it.
///
/// A link-local peer is reachable only through the interface on its link, and indexes are per
/// host, so a peer's own zone is useless here.
#[cfg(unix)]
#[must_use]
pub fn ipv6_scope_holding(address: Ipv6Addr) -> Option<u32> {
    nix::ifaddrs::getifaddrs().ok()?.find_map(|interface| {
        let held = interface.address?.as_sockaddr_in6()?.ip();
        (held == address)
            .then(|| nix::net::if_::if_nametoindex(interface.interface_name.as_str()).ok())
            .flatten()
    })
}

/// Limit: interface lookup is Unix-only; add a Win32 `GetAdaptersAddresses` path when Windows
/// clients need link-local remotes.
#[cfg(not(unix))]
#[must_use]
pub const fn ipv6_scope_holding(_address: Ipv6Addr) -> Option<u32> {
    None
}
