#![cfg(test)]
#![cfg(target_os = "linux")]
#![expect(
    clippy::unused_self,
    reason = "zbus interface methods take &self even when the fixture object answers with a constant."
)]

use std::{
    io::{BufRead as _, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};

use atspi_proxies::common::{
    CoordType, Interface, InterfaceSet, ObjectRef, ObjectRefOwned, Role, State, StateSet,
};
use bootty_computer::{
    Computer, ComputerAccess, ComputerAction, ComputerElement, ComputerError, ComputerResult,
    PressMethod,
};
use pretty_assertions::assert_eq;
use rstest::rstest;
use zbus::blocking::Connection;

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// A private bus and subprocess keep test environment changes away from other tests and the host.
#[rstest]
fn adapter_semantics() {
    if std::env::var_os("BOOTTY_ATSPI_FIXTURE").is_some() {
        exercise_adapter();
        return;
    }
    let mut bus = Process(
        Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut address = String::new();
    BufReader::new(bus.0.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "adapter_semantics", "--nocapture"])
        .env("BOOTTY_ATSPI_FIXTURE", "1")
        .env("DBUS_SESSION_BUS_ADDRESS", address.trim())
        .env_remove("DISPLAY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(Default)]
struct Values {
    text: String,
    presses: usize,
    reject: bool,
    replaced: bool,
    disabled: bool,
    extra: bool,
    closed: bool,
    foreign: Option<ObjectRefOwned>,
}

#[derive(Clone)]
struct Accessible {
    name: &'static str,
    bus: zbus::names::OwnedUniqueName,
    values: Arc<Mutex<Values>>,
}

impl Accessible {
    fn object(&self, name: &str) -> ObjectRefOwned {
        ObjectRef::new_owned(
            self.bus.clone(),
            zbus::zvariant::ObjectPath::try_from(format!("/org/a11y/atspi/accessible/{name}"))
                .unwrap(),
        )
    }
    fn children(&self) -> Vec<ObjectRefOwned> {
        match self.name {
            "root" => vec![self.object("app")],
            "app" if !self.values.lock().unwrap().closed => vec![self.object("window")],
            "window" => {
                let state = self.values.lock().unwrap();
                let mut children = vec![
                    self.object(if state.replaced {
                        "replacement"
                    } else {
                        "button"
                    }),
                    self.object("entry"),
                    self.object("readonly"),
                    self.object("password"),
                    self.object("label"),
                ];
                if state.extra {
                    children.extend((0..350).map(|_| self.object("label")));
                }
                children.extend(state.foreign.clone());
                children
            }
            _ => Vec::new(),
        }
    }
}

#[zbus::interface(name = "org.a11y.atspi.Accessible")]
impl Accessible {
    fn get_children(&self) -> Vec<ObjectRefOwned> {
        self.children()
    }
    fn get_child_at_index(&self, index: i32) -> zbus::fdo::Result<ObjectRefOwned> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.children().get(index).cloned())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("child index".into()))
    }
    fn get_role(&self) -> Role {
        match self.name {
            "app" => Role::Application,
            "root" | "window" => Role::Frame,
            "button" | "replacement" => Role::Button,
            "password" => Role::PasswordText,
            "label" => Role::Label,
            _ => Role::Entry,
        }
    }
    fn get_state(&self) -> StateSet {
        let mut states: StateSet = [State::Showing, State::Visible, State::Sensitive]
            .into_iter()
            .collect();
        if self.name == "entry" {
            states.insert(State::Editable);
        }
        if self.name == "readonly" {
            states.insert(State::ReadOnly);
        }
        if matches!(self.name, "button" | "replacement") && self.values.lock().unwrap().disabled {
            states.remove(State::Sensitive);
        }
        states
    }
    fn get_interfaces(&self) -> InterfaceSet {
        let mut interfaces = InterfaceSet::new(Interface::Accessible | Interface::Component);
        if matches!(self.name, "button" | "replacement") {
            interfaces.insert(Interface::Action);
        }
        if matches!(self.name, "entry" | "readonly" | "password") {
            interfaces.insert(Interface::Text);
            interfaces.insert(Interface::EditableText);
        }
        interfaces
    }
    #[zbus(property)]
    fn child_count(&self) -> i32 {
        i32::try_from(self.children().len()).unwrap()
    }
    #[zbus(property)]
    fn accessible_id(&self) -> &'static str {
        "org.bootty.AtspiFixture"
    }
    #[zbus(property)]
    fn name(&self) -> &str {
        if self.name == "replacement" {
            "button"
        } else {
            self.name
        }
    }
}

struct Component;
#[zbus::interface(name = "org.a11y.atspi.Component")]
impl Component {
    fn get_extents(&self, coords: CoordType) -> (i32, i32, i32, i32) {
        assert_eq!(coords, CoordType::Screen);
        (0, 0, 400, 300)
    }
    fn grab_focus(&self) -> zbus::fdo::Result<bool> {
        Err(zbus::fdo::Error::Failed("must never focus".into()))
    }
}

struct Action(Arc<Mutex<Values>>);
#[zbus::interface(name = "org.a11y.atspi.Action")]
impl Action {
    #[zbus(property, name = "NActions")]
    fn n_actions(&self) -> i32 {
        1
    }
    fn get_name(&self, index: i32) -> &'static str {
        assert_eq!(index, 0);
        "click"
    }
    fn do_action(&self, index: i32) -> bool {
        assert_eq!(index, 0);
        let mut state = self.0.lock().unwrap();
        state.presses = state.presses.saturating_add(1);
        !state.reject
    }
}

struct Text(Arc<Mutex<Values>>);
#[zbus::interface(name = "org.a11y.atspi.Text")]
impl Text {
    fn get_text(&self, start: i32, end: i32) -> String {
        let _ = (start, end);
        self.0.lock().unwrap().text.clone()
    }
}
struct Editable(Arc<Mutex<Values>>);
#[zbus::interface(name = "org.a11y.atspi.EditableText")]
impl Editable {
    fn set_text_contents(&self, value: &str) -> bool {
        self.0.lock().unwrap().text = value.into();
        true
    }
}
struct Bus(String);
#[zbus::interface(name = "org.a11y.Bus")]
impl Bus {
    fn get_address(&self) -> &str {
        &self.0
    }
}

fn fixture() -> (Connection, Connection, Arc<Mutex<Values>>) {
    let server = Connection::session().unwrap();
    server.request_name("org.a11y.Bus").unwrap();
    server.request_name("org.a11y.atspi.Registry").unwrap();
    server
        .object_server()
        .at(
            "/org/a11y/bus",
            Bus(std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap()),
        )
        .unwrap();
    let values = Arc::new(Mutex::new(Values::default()));
    let foreign = Connection::session().unwrap();
    // Querying this other owner's deliberately absent control would fail the observation.
    values.lock().unwrap().foreign = Some(ObjectRef::new_owned(
        foreign.unique_name().unwrap().to_owned(),
        zbus::zvariant::ObjectPath::try_from("/foreign_ungranted_control")
            .unwrap()
            .to_owned(),
    ));
    for name in [
        "root",
        "app",
        "window",
        "button",
        "replacement",
        "entry",
        "readonly",
        "password",
        "label",
    ] {
        let path = format!("/org/a11y/atspi/accessible/{name}");
        server
            .object_server()
            .at(
                path.as_str(),
                Accessible {
                    name,
                    bus: server.unique_name().unwrap().to_owned(),
                    values: values.clone(),
                },
            )
            .unwrap();
        server.object_server().at(path.as_str(), Component).unwrap();
        if matches!(name, "button" | "replacement") {
            server
                .object_server()
                .at(path.as_str(), Action(values.clone()))
                .unwrap();
        }
        if matches!(name, "entry" | "readonly" | "password") {
            server
                .object_server()
                .at(path.as_str(), Text(values.clone()))
                .unwrap();
            server
                .object_server()
                .at(path.as_str(), Editable(values.clone()))
                .unwrap();
        }
    }
    (server, foreign, values)
}

fn exercise_adapter() {
    let (_server, _foreign, values) = fixture();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let access = ComputerAccess::from_user_setting(true);
        let computer = Computer::new();
        let targets = computer.targets().await.unwrap();
        assert_eq!(targets.len(), 1);
        let target = targets.first().unwrap();
        assert!(target.native_id.is_some());
        assert!(target.is_semantic_only());
        assert_eq!(target.process_id, i32::try_from(std::process::id()).unwrap());
        let elements = observe(&computer, &access, target).await;
        let find = |name: &str| elements.iter().find(|element| element.name.as_deref() == Some(name)).unwrap().clone();
        let button = find("button");
        let press = |element: ComputerElement| ComputerAction::Press { element: Box::new(element), allow_click: false };
        // A fresh adapter must resolve the exact same token; application commands recreate it.
        assert_eq!(Computer::new().execute(&access, target, &press(button.clone())).await.unwrap(), ComputerResult::Pressed { method: PressMethod::Action });
        assert_eq!(values.lock().unwrap().presses, 1);
        for text in ["semantic 🥟", "", "a\nb"] {
            computer.execute(&access, target, &ComputerAction::SetValue { element: Box::new(find("entry")), value: text.into() }).await.unwrap();
            let current = observe(&computer, &access, target).await;
            assert_eq!(current.iter().find(|element| element.name.as_deref() == Some("entry")).and_then(|element| element.value.as_deref()), if text.is_empty() { None } else { Some(text) });
            assert!(current.iter().find(|element| element.name.as_deref() == Some("password")).unwrap().value.is_none());
        }
        let readonly = computer.execute(&access, target, &ComputerAction::SetValue { element: Box::new(find("readonly")), value: "refused".into() }).await;
        assert!(matches!(readonly, Err(ComputerError::InvalidAction(_))));
        assert_eq!(values.lock().unwrap().text, "a\nb");
        assert!(matches!(computer.execute(&access, target, &press(find("label"))).await, Err(ComputerError::InvalidAction(_))));
        assert!(matches!(computer.execute(&access, target, &ComputerAction::Press { element: Box::new(find("label")), allow_click: true }).await, Err(ComputerError::Unsupported)));
        values.lock().unwrap().reject = true;
        assert!(matches!(computer.execute(&access, target, &press(button.clone())).await, Err(ComputerError::Platform(_))));
        assert_eq!(values.lock().unwrap().presses, 2, "A rejected action is never retried or clicked");
        values.lock().unwrap().disabled = true;
        assert!(matches!(computer.execute(&access, target, &press(button.clone())).await, Err(ComputerError::InvalidAction(_))));
        values.lock().unwrap().disabled = false;
        values.lock().unwrap().replaced = true;
        assert!(matches!(computer.execute(&access, target, &press(button)).await, Err(ComputerError::StaleElement)));
        assert_eq!(values.lock().unwrap().presses, 2);
        let mut stale = target.clone();
        stale.native_id = Some("atspi::0.99/replaced".into());
        assert!(matches!(computer.execute(&access, &stale, &ComputerAction::Elements).await, Err(ComputerError::TargetUnavailable)));
        assert!(matches!(computer.execute(&access, target, &ComputerAction::Focus).await, Err(ComputerError::Unsupported)));
        assert!(matches!(computer.execute(&access, target, &ComputerAction::Snapshot).await, Err(ComputerError::Unsupported)));
        values.lock().unwrap().extra = true;
        let result = computer.execute(&access, target, &ComputerAction::Elements).await.unwrap();
        assert!(matches!(result, ComputerResult::Elements { omitted: true, elements, .. } if elements.len() <= bootty_computer::MAX_ELEMENTS));
        values.lock().unwrap().closed = true;
        assert!(matches!(computer.execute(&access, target, &ComputerAction::Elements).await, Err(ComputerError::TargetUnavailable)));
    });
}

async fn observe(
    computer: &Computer,
    access: &ComputerAccess,
    target: &bootty_computer::ComputerTarget,
) -> Vec<ComputerElement> {
    let ComputerResult::Elements {
        elements, omitted, ..
    } = computer
        .execute(access, target, &ComputerAction::Elements)
        .await
        .unwrap()
    else {
        panic!("expected elements")
    };
    assert!(!omitted);
    elements
}
