use crate::{AddressError, normalize_address};
use url::{Host, Url};

/// Exact origin allowed to receive a saved login. HTTP is limited to local development.
///
/// # Errors
/// Rejects unsafe addresses and non-loopback unencrypted websites.
pub fn login_origin(address: &str) -> Result<String, AddressError> {
    let address = normalize_address(address)?;
    let url = Url::parse(&address).map_err(|_| AddressError::Invalid)?;
    let local = match url.host() {
        Some(Host::Domain("localhost")) => true,
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if url.scheme() != "https" && !(url.scheme() == "http" && local) {
        return Err(AddressError::UnsupportedScheme);
    }
    Ok(url.origin().ascii_serialization())
}
