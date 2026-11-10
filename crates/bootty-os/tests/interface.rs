//! Local interface lookup for link-local remote addresses.

use bootty_os::interface::ipv6_scope_holding;
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::net::Ipv6Addr;

/// Addresses reserved for documentation and an unassigned link-local host are never local.
#[rstest]
#[case::documentation("2001:db8::1")]
#[case::unassigned_link_local("fe80::dead:beef:cafe:1")]
fn an_address_no_interface_holds_has_no_scope(#[case] address: Ipv6Addr) {
    assert_eq!(ipv6_scope_holding(address), None);
}

/// macOS loopback always holds `::1` and the link-local `fe80::1`, so both name one interface.
#[cfg(target_os = "macos")]
#[rstest]
fn a_held_link_local_address_resolves_to_its_interface() {
    let loopback = ipv6_scope_holding(Ipv6Addr::LOCALHOST);
    assert!(loopback.is_some(), "loopback holds ::1");
    assert_eq!(
        ipv6_scope_holding("fe80::1".parse().expect("link-local address")),
        loopback
    );
}
