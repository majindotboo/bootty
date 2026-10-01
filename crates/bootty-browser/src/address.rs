use thiserror::Error;
use url::Url;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AddressError {
    #[error("Enter a web address.")]
    Empty,
    #[error("Use an HTTP or HTTPS web address.")]
    UnsupportedScheme,
    #[error("This web address is invalid.")]
    Invalid,
    #[error("Use the website's sign-in page instead of credentials in the address.")]
    Credentials,
}

/// Bare local development addresses use HTTP; other bare hosts use HTTPS.
///
/// # Errors
/// Rejects invalid addresses, unsupported schemes, and embedded credentials.
pub fn normalize_address(address: &str) -> Result<String, AddressError> {
    let address = address.trim();
    if address.is_empty() {
        return Err(AddressError::Empty);
    }
    if address == "about:blank" {
        return Ok(address.to_owned());
    }
    if address.chars().any(char::is_whitespace) {
        return Err(AddressError::Invalid);
    }
    let host = address.split('/').next().unwrap_or_default();
    let local = host == "localhost"
        || host.starts_with("localhost:")
        || host == "127.0.0.1"
        || host.starts_with("127.0.0.1:")
        || host == "[::1]"
        || host.starts_with("[::1]:");
    let candidate = if address.contains("://") {
        address.to_owned()
    } else if local {
        format!("http://{address}")
    } else if host.rsplit_once(':').is_some_and(|(name, port)| {
        name.contains('.') && !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit())
    }) {
        format!("https://{address}")
    } else if host.contains(':') {
        return Err(AddressError::UnsupportedScheme);
    } else {
        format!("https://{address}")
    };
    let url = Url::parse(&candidate).map_err(|_| AddressError::Invalid)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AddressError::UnsupportedScheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AddressError::Credentials);
    }
    if url.host_str().is_none() {
        return Err(AddressError::Invalid);
    }
    Ok(url.into())
}
