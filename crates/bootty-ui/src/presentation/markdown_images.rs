//! Where a reply's markdown image lives when it's a file on the conversation's host.

/// The host path of a markdown image `url`, when it names a file.
///
/// An absolute path is as written, a `file://` URL is its path, and a relative path is from the
/// conversation's directory `cwd`. Web and data URLs, and paths from a home directory this
/// computer can't name, aren't files to read here.
#[must_use]
pub fn markdown_image_path(url: &str, cwd: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty()
        || url.starts_with('~')
        || ["http://", "https://", "data:"].iter().any(|scheme| {
            url.get(..scheme.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
        })
    {
        return None;
    }
    if let Some(path) = url.strip_prefix("file://") {
        return percent_decode(path.strip_prefix("localhost").unwrap_or(path));
    }
    let windows_absolute = url.as_bytes().get(1) == Some(&b':')
        && url.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    if url.starts_with('/') || windows_absolute {
        return Some(url.to_owned());
    }
    let relative = url.trim_start_matches("./");
    Some(format!("{}/{relative}", cwd.trim_end_matches('/')))
}

/// `text` with `%XX` escapes decoded, or `None` when they don't make UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        if byte == b'%'
            && let Some(hex) = after.get(..2)
            && let Ok(value) = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16)
        {
            bytes.push(value);
            rest = after.get(2..).unwrap_or_default();
        } else {
            bytes.push(byte);
            rest = after;
        }
    }
    String::from_utf8(bytes).ok()
}
