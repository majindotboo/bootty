//! Bounded OSC 5522 image assembly. Permission and destination ownership belong to the host.
use base64::{Engine as _, engine::general_purpose::STANDARD};
pub const MAX_PACKET: usize = 8192;
pub const MAX_IMAGE: usize = 16 * 1024 * 1024;
#[derive(Debug, PartialEq, Eq)]
pub struct ImageWrite {
    pub id: String,
    pub mime: String,
    pub data: Vec<u8>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum ClipboardResult {
    Reply(Vec<u8>),
    Image(ImageWrite),
}
#[derive(Default)]
pub struct ClipboardWrite {
    active: Option<ImageWrite>,
}
impl ClipboardWrite {
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active.is_some()
    }
    pub fn reset(&mut self) {
        self.active = None;
    }
    pub fn feed(&mut self, packet: &[u8], allowed: bool) -> Option<ClipboardResult> {
        if packet.len() > MAX_PACKET {
            return self.fail("EINVAL");
        }
        let Ok(packet) = std::str::from_utf8(packet) else {
            return self.fail("EINVAL");
        };
        let (metadata, data) = packet.split_once(';').unwrap_or((packet, ""));
        let field = |key| {
            metadata
                .split(':')
                .filter_map(|s| s.split_once('='))
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v)
        };
        let id = field("id").unwrap_or("");
        if id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.+".contains(&b))
        {
            return self.fail("EINVAL");
        }
        match field("type") {
            Some("read") => Some(ClipboardResult::Reply(reply("read", id, "ENOSYS"))),
            Some("write") => {
                self.reset();
                let error = if !allowed {
                    Some("EPERM")
                } else if field("loc").is_some_and(|s| s != "clipboard") {
                    Some("ENOSYS")
                } else {
                    None
                };
                if let Some(error) = error {
                    return Some(ClipboardResult::Reply(reply("write", id, error)));
                }
                self.active = Some(ImageWrite {
                    id: id.into(),
                    mime: String::new(),
                    data: Vec::new(),
                });
                None
            }
            Some("wdata") => {
                self.active.as_ref()?;
                if !allowed {
                    return self.fail("EPERM");
                }
                let mime = field("mime");
                if mime.is_none() && data.is_empty() {
                    let image = self.active.take()?;
                    return Some(if image.data.is_empty() {
                        ClipboardResult::Reply(reply("write", &image.id, "EINVAL"))
                    } else {
                        ClipboardResult::Image(image)
                    });
                }
                let mime = mime
                    .and_then(|m| STANDARD.decode(m).ok())
                    .and_then(|m| String::from_utf8(m).ok());
                let Some(mime) = mime.filter(|m| {
                    matches!(
                        m.as_str(),
                        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp"
                    )
                }) else {
                    return self.fail("EINVAL");
                };
                let Ok(chunk) = STANDARD.decode(data) else {
                    return self.fail("EINVAL");
                };
                let active = self.active.as_mut()?;
                if chunk.is_empty()
                    || chunk.len() > 4096
                    || active.data.len().saturating_add(chunk.len()) > MAX_IMAGE
                    || (!active.mime.is_empty() && active.mime != mime)
                {
                    return self.fail("EINVAL");
                }
                active.mime = mime;
                active.data.extend(chunk);
                None
            }
            Some("walias") => self.fail("ENOSYS"),
            _ => self.fail("EINVAL"),
        }
    }
    fn fail(&mut self, status: &str) -> Option<ClipboardResult> {
        self.active
            .take()
            .map(|image| ClipboardResult::Reply(reply("write", &image.id, status)))
    }
}
pub fn reply(kind: &str, id: &str, status: &str) -> Vec<u8> {
    let id: String = id
        .bytes()
        .take(128)
        .filter(|b| b.is_ascii_alphanumeric() || b"-_.+".contains(b))
        .map(char::from)
        .collect();
    format!("\x1b]5522;type={kind}:status={status}:id={id}\x1b\\").into_bytes()
}
