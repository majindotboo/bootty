mod address;
mod annotation_images;
mod annotation_protocol;
mod annotations;
mod credential;
mod credential_store;
mod credentials;
mod input;
#[cfg(target_os = "linux")]
mod linux_host;
mod native;
mod profile;
mod snapshot;

pub use address::{AddressError, normalize_address, resolve_address};
pub use annotation_images::{
    AnnotationCaptureContext, AnnotationImage, AnnotationImageGeometry, AnnotationRect,
};
pub use annotation_protocol::AnnotationEvent;
pub use annotations::{
    Annotation, AnnotationAnchor, AnnotationError, AnnotationSelection, AnnotationStore,
    annotation_batch, apply_annotation_event,
};
pub use credential::{
    CredentialAccount, CredentialDecision, CredentialError, CredentialTarget, SecretPassword,
    WebOrigin,
};
pub use credential_store::PlatformCredentialStore;
pub use credentials::{CredentialStore, Credentials};
pub use input::{BrowserInput, BrowserModifier, BrowserMouseButton};
pub use native::{
    BrowserBounds, BrowserEvent, BrowserShortcut, BrowserView, NativeBrowserError,
    poll_platform_events,
};
pub use profile::{BrowserProfile, SiteDataReset};
pub use snapshot::{BrowserDocumentSnapshot, valid_document_token};
