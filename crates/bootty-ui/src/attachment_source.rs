//! Temporary files behind an attachment until its import finishes.
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum AttachmentTemporary {
    Clipboard(Arc<tempfile::NamedTempFile>),
}
