//! AT-SPI uses the accessibility bus directly; no helper process, focus request or input events.
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};

use atspi_proxies::{
    accessible::AccessibleProxyBlocking,
    action::ActionProxyBlocking,
    bus::BusProxyBlocking,
    common::{CoordType, Interface, ObjectRef, ObjectRefOwned, Role, State},
    component::ComponentProxyBlocking,
    editable_text::EditableTextProxyBlocking,
    text::TextProxyBlocking,
};
use zbus::{blocking::Connection, proxy::CacheProperties};

use crate::{
    ComputerElement, ComputerError, ComputerTarget, DisplayBounds, ElementRoot, ElementState,
    MAX_ELEMENT_DEPTH, desktop::NativeWindow,
};

const CALL_TIMEOUT: Duration = Duration::from_millis(500);
const BUDGET: Duration = Duration::from_secs(3);
// Bound tree work independently of the returned controls. Raise this for a real app that needs it.
const MAX_READS: usize = 4_000;

struct Session {
    connection: Connection,
    started: Instant,
}

impl Session {
    fn new() -> Result<Self, ComputerError> {
        let started = Instant::now();
        let session = zbus::blocking::connection::Builder::session()
            .map_err(platform)?
            .method_timeout(CALL_TIMEOUT)
            .build()
            .map_err(platform)?;
        let address = BusProxyBlocking::new(&session)
            .and_then(|bus| bus.get_address())
            .map_err(platform)?;
        let connection = zbus::blocking::connection::Builder::address(address.as_str())
            .map_err(platform)?
            .method_timeout(CALL_TIMEOUT)
            .build()
            .map_err(platform)?;
        Ok(Self {
            connection,
            started,
        })
    }

    fn check(&self) -> Result<(), ComputerError> {
        if self.started.elapsed() >= BUDGET {
            Err(ComputerError::Timeout)
        } else {
            Ok(())
        }
    }

    fn accessible<'a>(
        &'a self,
        object: &'a ObjectRefOwned,
    ) -> Result<AccessibleProxyBlocking<'a>, ComputerError> {
        self.check()?;
        AccessibleProxyBlocking::builder(&self.connection)
            .destination(object.name().ok_or(ComputerError::StaleElement)?.as_str())
            .map_err(platform)?
            .path(object.path())
            .map_err(platform)?
            .cache_properties(CacheProperties::No)
            .build()
            .map_err(platform)
    }

    fn bounds(&self, object: &ObjectRefOwned) -> Result<DisplayBounds, ComputerError> {
        self.check()?;
        let component = ComponentProxyBlocking::builder(&self.connection)
            .destination(object.name().ok_or(ComputerError::StaleElement)?.as_str())
            .map_err(platform)?
            .path(object.path())
            .map_err(platform)?
            .cache_properties(CacheProperties::No)
            .build()
            .map_err(platform)?;
        let (x, y, width, height) = component.get_extents(CoordType::Screen).map_err(platform)?;
        // Toolkits use MIN as an offscreen/unknown sentinel. Wayland may instead report origin 0;
        // those frames are useful within the tree, but never authorize capture or pointer input.
        if x == i32::MIN || y == i32::MIN || width <= 0 || height <= 0 {
            return Err(ComputerError::TargetUnavailable);
        }
        Ok(DisplayBounds {
            x: f64::from(x),
            y: f64::from(y),
            width: f64::from(width),
            height: f64::from(height),
        })
    }

    fn windows(&self) -> Result<Vec<NativeWindow>, ComputerError> {
        let registry = AccessibleProxyBlocking::builder(&self.connection)
            .destination("org.a11y.atspi.Registry")
            .map_err(platform)?
            .cache_properties(CacheProperties::No)
            .build()
            .map_err(platform)?;
        let applications = registry.get_children().map_err(platform)?;
        let dbus = zbus::blocking::fdo::DBusProxy::new(&self.connection).map_err(platform)?;
        let mut windows = Vec::new();
        for application in applications.into_iter().take(256) {
            if self.check().is_err() {
                break;
            }
            let Some(name) = application.name() else {
                continue;
            };
            let Ok(pid) = dbus.get_connection_unix_process_id(name.clone().into()) else {
                continue;
            };
            let Ok(pid) = i32::try_from(pid) else {
                continue;
            };
            let Some(launch_time) = super::start_time(pid) else {
                continue;
            };
            let Ok(app) = self.accessible(&application) else {
                continue;
            };
            let bundle_id = app
                .accessible_id()
                .ok()
                .filter(|id| !id.is_empty())
                .or_else(|| {
                    std::fs::read_link(format!("/proc/{pid}/exe"))
                        .ok()?
                        .file_stem()?
                        .to_str()
                        .map(str::to_owned)
                });
            let Some(bundle_id) =
                bundle_id.filter(|id| id.len() <= 255 && !id.chars().any(char::is_control))
            else {
                continue;
            };
            let application_name = bounded(app.name().ok());
            let Ok(count) = app.child_count() else {
                continue;
            };
            for index in 0..count.min(256) {
                if self.check().is_err() || windows.len() == 256 {
                    break;
                }
                let Ok(object) = app.get_child_at_index(index) else {
                    continue;
                };
                // Embedded accessibles from another bus owner do not inherit this app's grant.
                if object.name() != application.name() {
                    continue;
                }
                if let Ok(window) = self.window(
                    &object,
                    pid,
                    launch_time,
                    &bundle_id,
                    application_name.as_deref(),
                ) {
                    windows.push(window);
                }
            }
        }
        Ok(windows)
    }

    fn window(
        &self,
        object: &ObjectRefOwned,
        process_id: i32,
        launch_time: f64,
        bundle_id: &str,
        application_name: Option<&str>,
    ) -> Result<NativeWindow, ComputerError> {
        let root = self.accessible(object)?;
        let role = root.get_role().map_err(platform)?;
        let state = root.get_state().map_err(platform)?;
        if !matches!(
            role,
            Role::Frame | Role::Dialog | Role::Window | Role::Alert
        ) || state.contains(State::Defunct)
            || !state.contains(State::Showing)
        {
            return Err(ComputerError::TargetUnavailable);
        }
        let bounds = self.bounds(object)?;
        let native_id = self.identity(object);
        if native_id.len() > 1024 {
            return Err(ComputerError::TargetUnavailable);
        }
        let mut hash = DefaultHasher::new();
        native_id.hash(&mut hash);
        // The full native_id, not this compact display id, proves window identity.
        let id = u32::try_from(hash.finish() & u64::from(u32::MAX))
            .unwrap_or(0)
            .max(1);
        let target = ComputerTarget {
            window_id: id,
            native_id: Some(native_id),
            process_id,
            bundle_id: bundle_id.into(),
            application_name: application_name.map(str::to_owned),
            launch_time,
            bounds: bounds.clone(),
            title: bounded(root.name().ok()),
        };
        Ok(NativeWindow {
            id,
            bounds,
            visible: true,
            app: Some(target),
        })
    }

    fn root(&self, window: &NativeWindow) -> Result<ObjectRefOwned, ComputerError> {
        let target = window
            .app
            .as_ref()
            .ok_or(ComputerError::TargetUnavailable)?;
        let id = target
            .native_id
            .as_deref()
            .ok_or(ComputerError::Unsupported)?;
        let (guid, object_id) = id
            .strip_prefix("atspi:")
            .and_then(|id| id.split_once(':'))
            .ok_or(ComputerError::TargetUnavailable)?;
        if guid != self.connection.server_guid() {
            return Err(ComputerError::StaleTarget);
        }
        let (name, path) = object_id
            .split_once('/')
            .ok_or(ComputerError::TargetUnavailable)?;
        let name = zbus::names::UniqueName::try_from(name.to_owned())
            .map_err(|_| ComputerError::TargetUnavailable)?;
        let path = zbus::zvariant::ObjectPath::try_from(format!("/{path}"))
            .map_err(|_| ComputerError::TargetUnavailable)?;
        let dbus = zbus::blocking::fdo::DBusProxy::new(&self.connection).map_err(platform)?;
        let pid = dbus
            .get_connection_unix_process_id(name.clone().into())
            .map_err(platform)?;
        if i32::try_from(pid).ok() != Some(target.process_id)
            || super::start_time(target.process_id).map(f64::to_bits)
                != Some(target.launch_time.to_bits())
        {
            return Err(ComputerError::StaleTarget);
        }
        let object = ObjectRef::new_owned(name, path);
        if self.bounds(&object)? != target.bounds {
            return Err(ComputerError::StaleTarget);
        }
        self.check()?;
        Ok(object)
    }

    fn resolve(
        &self,
        root: &ObjectRefOwned,
        path: &[u16],
    ) -> Result<Option<ObjectRefOwned>, ComputerError> {
        let mut current = root.clone();
        for index in path {
            let accessible = self.accessible(&current)?;
            if i32::from(*index) >= accessible.child_count().map_err(platform)? {
                return Ok(None);
            }
            let child = accessible
                .get_child_at_index(i32::from(*index))
                .map_err(platform)?;
            if child.is_null() || child.name() != root.name() {
                return Ok(None);
            }
            drop(accessible);
            current = child;
        }
        Ok(Some(current))
    }

    fn describe(
        &self,
        object: &ObjectRefOwned,
        path: Vec<u16>,
    ) -> Result<Option<ComputerElement>, ComputerError> {
        let accessible = self.accessible(object)?;
        let state = accessible.get_state().map_err(platform)?;
        if state.contains(State::Defunct) || !state.contains(State::Showing) {
            return Ok(None);
        }
        let Ok(frame) = self.bounds(object) else {
            return Ok(None);
        };
        let role = accessible.get_role().map_err(platform)?;
        let name = bounded(accessible.name().ok());
        let interfaces = accessible.get_interfaces().map_err(platform)?;
        let value = if role != Role::PasswordText && interfaces.contains(Interface::Text) {
            self.check()?;
            let text = TextProxyBlocking::builder(&self.connection)
                .destination(object.name().ok_or(ComputerError::StaleElement)?.as_str())
                .map_err(platform)?
                .path(object.path())
                .map_err(platform)?
                .cache_properties(CacheProperties::No)
                .build()
                .map_err(platform)?;
            bounded(Some(text.get_text(0, 200).map_err(platform)?))
        } else {
            None
        };
        let identifier = self.identity(object);
        if identifier.len() > 1024 {
            return Ok(None);
        }
        let mut states = Vec::new();
        if !state.contains(State::Sensitive) {
            states.push(ElementState::Disabled);
        }
        for (native, public) in [
            (State::Selected, ElementState::Selected),
            (State::Checked, ElementState::Checked),
            (State::Focused, ElementState::Focused),
        ] {
            if state.contains(native) {
                states.push(public);
            }
        }
        Ok(Some(ComputerElement {
            path,
            root: ElementRoot::Window,
            role: role.name().into(),
            name,
            value,
            identifier: Some(identifier),
            frame: Some(frame),
            states,
        }))
    }

    fn actionable(
        &self,
        object: &ObjectRefOwned,
    ) -> Result<atspi_proxies::common::InterfaceSet, ComputerError> {
        let accessible = self.accessible(object)?;
        let state = accessible.get_state().map_err(platform)?;
        if state.contains(State::Defunct) {
            return Err(ComputerError::StaleElement);
        }
        if !state.contains(State::Sensitive) || !state.contains(State::Showing) {
            return Err(ComputerError::InvalidAction(
                "the element is disabled or hidden".into(),
            ));
        }
        accessible.get_interfaces().map_err(platform)
    }

    fn identity(&self, object: &ObjectRefOwned) -> String {
        format!(
            "atspi:{}:{}{}",
            self.connection.server_guid(),
            object.name_as_str().unwrap_or_default(),
            object.path_as_str()
        )
    }
}

pub(super) fn available() -> bool {
    Session::new().is_ok()
}

pub(super) fn windows() -> Vec<NativeWindow> {
    Session::new()
        .and_then(|session| session.windows())
        .unwrap_or_default()
}

struct Listing<'a> {
    session: &'a Session,
    root: &'a ObjectRefOwned,
    limit: usize,
    reads: usize,
    elements: Vec<ComputerElement>,
    omitted: bool,
}

impl Listing<'_> {
    fn visit(&mut self, parent: &ObjectRefOwned, path: &[u16]) -> Result<(), ComputerError> {
        let accessible = self.session.accessible(parent)?;
        let count = accessible.child_count().map_err(platform)?;
        for index in 0..count {
            if self.session.check().is_err()
                || self.reads >= MAX_READS
                || self.elements.len() >= self.limit
                || path.len() >= MAX_ELEMENT_DEPTH
            {
                self.omitted = true;
                break;
            }
            let Ok(index) = u16::try_from(index) else {
                self.omitted = true;
                break;
            };
            self.reads = self.reads.saturating_add(1);
            let child = accessible
                .get_child_at_index(i32::from(index))
                .map_err(platform)?;
            if child.is_null() || child.name() != self.root.name() {
                continue;
            }
            let mut child_path = path.to_vec();
            child_path.push(index);
            if let Some(element) = self.session.describe(&child, child_path.clone())? {
                self.elements.push(element);
            }
            self.visit(&child, &child_path)?;
        }
        Ok(())
    }
}

pub(super) fn elements(
    window: &NativeWindow,
    limit: usize,
) -> Result<(Vec<ComputerElement>, bool), ComputerError> {
    let session = Session::new()?;
    let root = session.root(window)?;
    let mut listing = Listing {
        session: &session,
        root: &root,
        limit,
        reads: 0,
        elements: Vec::new(),
        omitted: false,
    };
    match listing.visit(&root, &[]) {
        Ok(()) => {}
        Err(ComputerError::Timeout) => listing.omitted = true,
        Err(error) => return Err(error),
    }
    Ok((listing.elements, listing.omitted))
}

pub(super) fn element(
    window: &NativeWindow,
    path: &[u16],
) -> Result<Option<ComputerElement>, ComputerError> {
    let session = Session::new()?;
    let root = session.root(window)?;
    let Some(object) = session.resolve(&root, path)? else {
        return Ok(None);
    };
    session.describe(&object, path.to_vec())
}

pub(super) fn press(window: &NativeWindow, path: &[u16]) -> Result<bool, ComputerError> {
    let session = Session::new()?;
    let root = session.root(window)?;
    let object = session
        .resolve(&root, path)?
        .ok_or(ComputerError::StaleElement)?;
    if !session.actionable(&object)?.contains(Interface::Action) {
        return Ok(false);
    }
    let action = ActionProxyBlocking::builder(&session.connection)
        .destination(object.name().ok_or(ComputerError::StaleElement)?.as_str())
        .map_err(platform)?
        .path(object.path())
        .map_err(platform)?
        .cache_properties(CacheProperties::No)
        .build()
        .map_err(platform)?;
    // The pinned proxy spells this property Nactions; the AT-SPI wire name is NActions.
    let count: i32 = action.inner().get_property("NActions").map_err(platform)?;
    for index in 0..count.min(16) {
        session.check()?;
        let name = action.get_name(index).map_err(platform)?;
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "click" | "press" | "activate" | "toggle" | "jump" | "open"
        ) {
            session.check()?;
            return if action.do_action(index).map_err(platform)? {
                Ok(true)
            } else {
                Err(ComputerError::Platform("AT-SPI rejected the press".into()))
            };
        }
    }
    Ok(false)
}

pub(super) fn set_value(
    window: &NativeWindow,
    path: &[u16],
    value: &str,
) -> Result<(), ComputerError> {
    let session = Session::new()?;
    let root = session.root(window)?;
    let object = session
        .resolve(&root, path)?
        .ok_or(ComputerError::StaleElement)?;
    if !session
        .actionable(&object)?
        .contains(Interface::EditableText)
    {
        return Err(ComputerError::Unsupported);
    }
    let state = session.accessible(&object)?.get_state().map_err(platform)?;
    if !state.contains(State::Editable) || state.contains(State::ReadOnly) {
        return Err(ComputerError::InvalidAction(
            "the element is read-only".into(),
        ));
    }
    let editable = EditableTextProxyBlocking::builder(&session.connection)
        .destination(object.name().ok_or(ComputerError::StaleElement)?.as_str())
        .map_err(platform)?
        .path(object.path())
        .map_err(platform)?
        .cache_properties(CacheProperties::No)
        .build()
        .map_err(platform)?;
    session.check()?;
    if editable.set_text_contents(value).map_err(platform)? {
        Ok(())
    } else {
        Err(ComputerError::Platform("AT-SPI rejected the value".into()))
    }
}

fn bounded(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.is_empty())
        .map(|text| text.chars().take(256).collect())
}

fn platform(error: impl std::fmt::Display) -> ComputerError {
    ComputerError::Platform(format!("AT-SPI: {error}"))
}
