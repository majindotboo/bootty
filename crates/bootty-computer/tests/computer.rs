use std::{
    io::Cursor,
    sync::{Arc, Mutex, PoisonError},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_computer::{
    Computer, ComputerAccess, ComputerAction, ComputerElement, ComputerError, ComputerResult,
    ComputerStatus, ComputerTarget, DisplayBounds, ElementState, HostCaptureRegion, Key,
    MouseButton, PermissionStatus, PressMethod,
    control::{MAX_TEXT_UNITS, text_pieces},
    desktop::{Desktop, Image, InputEvent, InputRoute, NativeWindow},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};

#[fixture]
fn target() -> ComputerTarget {
    ComputerTarget {
        window_id: 42,
        native_id: None,
        backend_id: None,
        display: None,
        process_id: 123,
        bundle_id: "dev.bootty.test-target".into(),
        application_name: None,
        launch_time: 1_000.0,
        bounds: DisplayBounds {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        },
        title: None,
    }
}

proptest! {
    #[test]
    fn numeric_window_ids_never_substitute_for_the_full_native_identity(suffix in "[a-z]{1,100}") {
        let mut observed = target();
        observed.native_id = Some(format!("atspi::1.1/{suffix}"));
        let fake = Fake::focused(&observed);
        let mut replaced = observed.clone();
        replaced.native_id = Some(format!("atspi::1.2/{suffix}"));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let result = runtime.block_on(run(&fake, &replaced, &ComputerAction::Elements));
        prop_assert!(matches!(result, Err(ComputerError::TargetUnavailable)));
        let decoded: ComputerTarget = serde_json::from_str(&serde_json::to_string(&observed)?)?;
        prop_assert_eq!(observed, decoded);
    }

    #[test]
    fn disabled_access_never_reaches_the_desktop(x in any::<f64>(), y in any::<f64>()) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let result = runtime.block_on(Fake::focused(&target()).computer().execute(
            &ComputerAccess::from_user_setting(false), &target(),
            &ComputerAction::Click { x, y, button: MouseButton::Left },
        ));
        prop_assert!(matches!(result, Err(ComputerError::Disabled)));
    }

    #[test]
    fn pointer_actions_outside_target_never_reach_the_desktop(x in -1_000_000.0..-0.01_f64, y in -1_000_000.0..1_000_000.0_f64) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let result = runtime.block_on(Fake::focused(&target()).computer().execute(
            &ComputerAccess::from_user_setting(true), &target(), &ComputerAction::Move { x, y },
        ));
        prop_assert!(matches!(result, Err(ComputerError::InvalidAction(_))));
    }

    // Window tokens are compared exactly, so a JSON hop through the host must not round floats.
    #[test]
    fn window_tokens_survive_json_exactly(
        launch_time in prop::num::f64::NORMAL,
        x in prop::num::f64::NORMAL,
        y in prop::num::f64::NORMAL,
    ) {
        let mut token = target();
        token.launch_time = launch_time;
        token.bounds.x = x;
        token.bounds.y = y;
        let decoded: ComputerTarget = serde_json::from_str(&serde_json::to_string(&token)?)?;
        prop_assert_eq!(decoded, token);
    }
}

#[rstest]
#[case(f64::NAN, 0.0)]
#[case(0.0, f64::INFINITY)]
#[case(800.0, 0.0)]
#[case(0.0, 600.0)]
#[tokio::test]
async fn rejects_invalid_coordinates_before_the_desktop(
    target: ComputerTarget,
    #[case] x: f64,
    #[case] y: f64,
) {
    let result = Fake::focused(&target)
        .computer()
        .execute(
            &ComputerAccess::from_user_setting(true),
            &target,
            &ComputerAction::Move { x, y },
        )
        .await;
    assert!(matches!(result, Err(ComputerError::InvalidAction(_))));
}

#[rstest]
#[case(0, 123, 800.0)]
#[case(42, 0, 800.0)]
#[case(42, 123, 0.0)]
#[case(42, 123, f64::INFINITY)]
#[tokio::test]
async fn rejects_invalid_target_before_the_desktop(
    mut target: ComputerTarget,
    #[case] window_id: u32,
    #[case] process_id: i32,
    #[case] width: f64,
) {
    target.window_id = window_id;
    target.process_id = process_id;
    target.bounds.width = width;
    let result = Fake::focused(&target)
        .computer()
        .execute(
            &ComputerAccess::from_user_setting(true),
            &target,
            &ComputerAction::Snapshot,
        )
        .await;
    assert!(matches!(result, Err(ComputerError::InvalidTarget)));
}

#[rstest]
#[case(String::new())]
#[case("a".repeat(4097))]
#[tokio::test]
async fn refuses_unbounded_text_before_the_desktop(target: ComputerTarget, #[case] text: String) {
    let result = Fake::focused(&target)
        .computer()
        .execute(
            &ComputerAccess::from_user_setting(true),
            &target,
            &ComputerAction::TypeText { text },
        )
        .await;
    assert!(matches!(result, Err(ComputerError::InvalidAction(_))));
}

proptest! {
    #[test]
    fn frame_local_regions_follow_negative_desktop_origins(
        origin_x in -10_000.0..10_000.0_f64,
        origin_y in -10_000.0..10_000.0_f64,
        local_x in 0_u32..700,
        local_y in 0_u32..500,
    ) {
        let mut target = target();
        target.bounds.x = origin_x;
        target.bounds.y = origin_y;
        let geometry = HostCaptureRegion {
            frame_width: 800.0, frame_height: 600.0,
            rect: DisplayBounds { x: f64::from(local_x), y: f64::from(local_y), width: 100.0, height: 100.0 },
        };
        let region = geometry.resolve(&target)?;
        prop_assert_eq!(region.x.to_bits(), (origin_x + f64::from(local_x)).to_bits());
        prop_assert_eq!(region.y.to_bits(), (origin_y + f64::from(local_y)).to_bits());
        prop_assert_eq!(region.width.to_bits(), 100.0_f64.to_bits());
        prop_assert_eq!(region.height.to_bits(), 100.0_f64.to_bits());
        prop_assert!(target.validate_action(&ComputerAction::SnapshotRegion { rect: region }).is_ok(), "valid contained region must be accepted");
    }
}

#[rstest]
#[case(-0.01, 0.0, 100.0, 100.0)]
#[case(0.0, -0.01, 100.0, 100.0)]
#[case(700.01, 0.0, 100.0, 100.0)]
#[case(0.0, 500.01, 100.0, 100.0)]
#[case(0.0, 0.0, 0.0, 100.0)]
#[case(f64::NAN, 0.0, 100.0, 100.0)]
#[tokio::test]
async fn invalid_capture_regions_never_reach_the_desktop(
    target: ComputerTarget,
    #[case] x: f64,
    #[case] y: f64,
    #[case] width: f64,
    #[case] height: f64,
) {
    let action = ComputerAction::SnapshotRegion {
        rect: DisplayBounds {
            x,
            y,
            width,
            height,
        },
    };
    let result = Fake::focused(&target)
        .computer()
        .execute(&ComputerAccess::from_user_setting(true), &target, &action)
        .await;
    assert!(matches!(result, Err(ComputerError::InvalidAction(_))));
}

#[rstest]
#[case(800.0, 600.0, true)]
#[case(799.0, 600.0, false)]
#[case(800.0, 601.0, false)]
fn captured_frame_resize_invalidates_region(
    target: ComputerTarget,
    #[case] frame_width: f64,
    #[case] frame_height: f64,
    #[case] accepted: bool,
) {
    let geometry = HostCaptureRegion {
        frame_width,
        frame_height,
        rect: DisplayBounds {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        },
    };
    pretty_assertions::assert_eq!(geometry.resolve(&target).is_ok(), accepted);
}

/// A desktop whose windows, focus and permissions a test controls. It records what reaches it.
#[derive(Default)]
struct Fake {
    independent: bool,
    entered: Option<std::sync::mpsc::Sender<()>>,
    resume: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    released: Option<std::sync::mpsc::Sender<()>>,
    trace: Mutex<Vec<&'static str>>,
    windows: Vec<NativeWindow>,
    /// Window-addressed input, as on macOS, unless a test says otherwise.
    route: Option<InputRoute>,
    /// The process and the bounds of the window that receives its keys.
    focus: Option<(i32, DisplayBounds)>,
    frontmost: Option<i32>,
    accessibility: Option<PermissionStatus>,
    recording: Option<PermissionStatus>,
    secure: bool,
    elements: Vec<ComputerElement>,
    /// Whether elements have their own press action.
    pressable: bool,
    posted: Mutex<Vec<InputEvent>>,
    raised: Mutex<Vec<u32>>,
    pressed: Mutex<Vec<Vec<u16>>>,
    values: Mutex<Vec<(Vec<u16>, String)>>,
}

impl Fake {
    /// The target is the only window, focused and in front, with one unlabeled button.
    fn focused(target: &ComputerTarget) -> Arc<Self> {
        Arc::new(Self {
            windows: vec![window(target)],
            focus: Some((target.process_id, target.bounds.clone())),
            frontmost: Some(target.process_id),
            elements: vec![button()],
            ..Self::default()
        })
    }

    fn computer(self: &Arc<Self>) -> Computer {
        Computer::with_desktop(self.clone())
    }

    fn posted(&self) -> Vec<InputEvent> {
        self.posted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn pressed(&self) -> Vec<Vec<u16>> {
        self.pressed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// An icon button with no label, centered on the point [`click`] uses.
fn button() -> ComputerElement {
    ComputerElement {
        path: vec![0, 2],
        role: "Button".into(),
        name: None,
        value: None,
        identifier: Some("back".into()),
        frame: DisplayBounds {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 20.0,
        },
        states: Vec::new(),
    }
}

fn press() -> ComputerAction {
    ComputerAction::Press {
        element: Box::new(button()),
        allow_click: true,
    }
}

fn window(target: &ComputerTarget) -> NativeWindow {
    NativeWindow {
        id: target.window_id,
        bounds: target.bounds.clone(),
        visible: true,
        app: Some(target.clone()),
    }
}

impl Desktop for Fake {
    fn status(&self) -> ComputerStatus {
        ComputerStatus {
            accessibility: self.accessibility.unwrap_or(PermissionStatus::Granted),
            screen_recording: self.recording.unwrap_or(PermissionStatus::Granted),
            secure_input: self.secure,
        }
    }

    fn windows(&self) -> Vec<NativeWindow> {
        self.windows.clone()
    }

    fn route(&self) -> InputRoute {
        self.route.unwrap_or(InputRoute::Window)
    }

    fn frontmost(&self) -> Option<i32> {
        self.frontmost
    }

    fn focus(&self, process_id: i32) -> Option<DisplayBounds> {
        self.focus
            .clone()
            .filter(|(owner, _)| *owner == process_id)
            .map(|(_, bounds)| bounds)
    }

    fn secure_input(&self) -> bool {
        self.secure
    }

    /// Each pixel encodes its own coordinates, so crops can be checked exactly.
    fn capture(
        &self,
        _: &NativeWindow,
        width: u32,
        height: u32,
        _: bool,
    ) -> Result<Image, ComputerError> {
        let rgba = (0..height)
            .flat_map(|row| (0..width).flat_map(move |column| pixel(column, row)))
            .collect();
        Ok(Image {
            width,
            height,
            rgba,
        })
    }

    fn post(&self, _: &NativeWindow, event: &InputEvent) -> Result<(), ComputerError> {
        if let Some(entered) = &self.entered {
            _ = entered.send(());
        }
        let resume = self
            .resume
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(resume) = resume {
            _ = resume.recv();
        }
        self.trace
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("posted");
        self.posted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event.clone());
        Ok(())
    }

    fn independent_input(&self) -> bool {
        self.independent
    }

    fn release_inputs(&self) -> Result<(), ComputerError> {
        self.trace
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("released");
        if let Some(released) = &self.released {
            _ = released.send(());
        }
        Ok(())
    }

    fn raise(&self, window: &NativeWindow) -> Result<(), ComputerError> {
        self.raised
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(window.id);
        Ok(())
    }

    fn elements(
        &self,
        _: &NativeWindow,
        limit: usize,
    ) -> Result<(Vec<ComputerElement>, bool), ComputerError> {
        Ok((
            self.elements.iter().take(limit).cloned().collect(),
            self.elements.len() > limit,
        ))
    }

    fn element(
        &self,
        _: &NativeWindow,
        path: &[u16],
    ) -> Result<Option<ComputerElement>, ComputerError> {
        Ok(self
            .elements
            .iter()
            .find(|element| element.path == path)
            .cloned())
    }

    fn pressable_at(
        &self,
        _: &NativeWindow,
        x: f64,
        y: f64,
    ) -> Result<Option<Vec<u16>>, ComputerError> {
        Ok(self
            .elements
            .iter()
            .find(|element| {
                self.pressable
                    && (element.frame.x..element.frame.x + element.frame.width).contains(&x)
                    && (element.frame.y..element.frame.y + element.frame.height).contains(&y)
            })
            .map(|element| element.path.clone()))
    }

    fn press(&self, _: &NativeWindow, path: &[u16]) -> Result<bool, ComputerError> {
        if self.pressable {
            self.pressed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(path.to_vec());
        }
        Ok(self.pressable)
    }

    fn set_value(&self, _: &NativeWindow, path: &[u16], value: &str) -> Result<(), ComputerError> {
        self.values
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((path.to_vec(), value.to_owned()));
        Ok(())
    }

    fn icon(&self, bundle_id: &str, side: u32) -> Option<Image> {
        (bundle_id == "dev.zed.Zed").then_some(())?;
        Some(Image {
            width: side,
            height: side,
            rgba: vec![255; usize::try_from(side.saturating_mul(side).saturating_mul(4)).ok()?],
        })
    }
}

const fn pixel(column: u32, row: u32) -> [u8; 4] {
    let [_, _, high, low] = column.to_be_bytes();
    let [_, _, row_high, row_low] = row.to_be_bytes();
    [
        high.wrapping_add(low),
        row_high.wrapping_add(row_low),
        low,
        255,
    ]
}

const fn click() -> ComputerAction {
    ComputerAction::Click {
        x: 10.0,
        y: 10.0,
        button: MouseButton::Left,
    }
}

fn type_text() -> ComputerAction {
    ComputerAction::TypeText { text: "hi".into() }
}

const fn key() -> ComputerAction {
    ComputerAction::Key {
        key: Key::Return,
        modifiers: Vec::new(),
    }
}

async fn run(
    fake: &Arc<Fake>,
    target: &ComputerTarget,
    action: &ComputerAction,
) -> Result<ComputerResult, ComputerError> {
    fake.computer()
        .execute(&ComputerAccess::from_user_setting(true), target, action)
        .await
}

#[rstest]
#[tokio::test]
async fn focused_uncovered_input_reaches_the_window(
    target: ComputerTarget,
    #[values(InputRoute::Window, InputRoute::Shared)] route: InputRoute,
) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.route = Some(route);
    let fake = Arc::new(fake);
    assert!(matches!(
        run(&fake, &target, &click()).await,
        Ok(ComputerResult::Posted)
    ));
    assert_eq!(
        fake.posted(),
        [InputEvent::Click {
            x: 10.0,
            y: 10.0,
            button: MouseButton::Left
        }]
    );
}

fn close(fake: &mut Fake, _: &ComputerTarget) {
    fake.windows.clear();
}
fn relaunch(fake: &mut Fake, _: &ComputerTarget) {
    if let Some(app) = fake
        .windows
        .first_mut()
        .and_then(|window| window.app.as_mut())
    {
        app.launch_time = 2_000.0;
    }
}
fn move_window(fake: &mut Fake, _: &ComputerTarget) {
    if let Some(app) = fake
        .windows
        .first_mut()
        .and_then(|window| window.app.as_mut())
    {
        app.bounds.x = 1.0;
    }
}
const fn focus_other_app(fake: &mut Fake, _: &ComputerTarget) {
    if let Some((process_id, _)) = fake.focus.as_mut() {
        *process_id = 999;
    }
}
const fn focus_other_window(fake: &mut Fake, _: &ComputerTarget) {
    if let Some((_, bounds)) = fake.focus.as_mut() {
        bounds.x = 100.0;
    }
}
const fn focus_nothing(fake: &mut Fake, _: &ComputerTarget) {
    fake.focus = None;
}
const fn leave_in_front(_: &mut Fake, _: &ComputerTarget) {}
const fn bring_other_app_to_front(fake: &mut Fake, _: &ComputerTarget) {
    fake.frontmost = Some(999);
}
fn cover_point(fake: &mut Fake, _: &ComputerTarget) {
    fake.windows.insert(
        0,
        NativeWindow {
            id: 7,
            bounds: DisplayBounds {
                x: 0.0,
                y: 0.0,
                width: 50.0,
                height: 50.0,
            },
            visible: true,
            app: None,
        },
    );
}
fn duplicate_frame(fake: &mut Fake, target: &ComputerTarget) {
    let mut twin = window(target);
    twin.id = 43;
    if let Some(app) = twin.app.as_mut() {
        app.window_id = 43;
    }
    fake.windows.push(twin);
}
const fn revoke_accessibility(fake: &mut Fake, _: &ComputerTarget) {
    fake.accessibility = Some(PermissionStatus::NotGranted);
}
const fn secure_input(fake: &mut Fake, _: &ComputerTarget) {
    fake.secure = true;
}

/// Each case changes one fact about the desktop; input must refuse and post nothing on every
/// route. A press of an element without its own action clicks, so it follows the same rules.
#[rstest]
#[case::window_closed(close)]
#[case::window_moved(move_window)]
#[case::process_relaunched(relaunch)]
#[case::accessibility_missing(revoke_accessibility)]
#[case::secure_input(secure_input)]
#[tokio::test]
async fn input_refuses_anything_but_the_exact_window(
    target: ComputerTarget,
    #[case] change: fn(&mut Fake, &ComputerTarget),
    #[values(click(), press(), type_text())] action: ComputerAction,
    #[values(InputRoute::Window, InputRoute::Shared)] route: InputRoute,
) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.route = Some(route);
    change(&mut fake, &target);
    let fake = Arc::new(fake);
    let result = run(&fake, &target, &action).await;
    assert!(result.is_err(), "input must be refused, got {result:?}");
    assert_eq!(fake.posted(), []);
}

/// Keys go wherever the target's process sends them, so on every route they need the target to
/// be that window, told apart from its process's other windows.
#[rstest]
#[case::other_app_focused(focus_other_app)]
#[case::other_window_focused(focus_other_window)]
#[case::nothing_focused(focus_nothing)]
#[case::two_windows_at_the_same_frame(duplicate_frame)]
#[tokio::test]
async fn keys_reach_only_the_window_its_process_focuses(
    target: ComputerTarget,
    #[case] change: fn(&mut Fake, &ComputerTarget),
    #[values(type_text(), key())] action: ComputerAction,
    #[values(InputRoute::Window, InputRoute::Shared)] route: InputRoute,
) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.route = Some(route);
    change(&mut fake, &target);
    let fake = Arc::new(fake);
    let result = run(&fake, &target, &action).await;
    assert!(
        matches!(result, Err(ComputerError::TargetNotFocused)),
        "{result:?}"
    );
    assert_eq!(fake.posted(), []);
}

/// A pointer event addressed to the window reaches it behind other apps and windows, so it needs
/// no focus. Shared pointer input lands on whatever is in front, so it needs both.
#[rstest]
#[case::other_app_focused(focus_other_app)]
#[case::point_covered(cover_point)]
#[case::two_windows_at_the_same_frame(duplicate_frame)]
#[tokio::test]
async fn only_shared_pointer_input_needs_focus_and_an_uncovered_point(
    target: ComputerTarget,
    #[case] change: fn(&mut Fake, &ComputerTarget),
    #[values(click(), press())] action: ComputerAction,
    #[values(InputRoute::Window, InputRoute::Shared)] route: InputRoute,
) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.route = Some(route);
    change(&mut fake, &target);
    let fake = Arc::new(fake);
    let result = run(&fake, &target, &action).await;
    let clicked = [InputEvent::Click {
        x: 10.0,
        y: 10.0,
        button: MouseButton::Left,
    }];
    if route == InputRoute::Window {
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(fake.posted(), clicked);
    } else {
        assert!(result.is_err(), "input must be refused, got {result:?}");
        assert_eq!(fake.posted(), []);
    }
}

/// Views such as web content ignore a click in a background app's window, so a left click there
/// presses the element under the point when it has its own press action. Clicks on the frontmost
/// app, other buttons, and points without such an element are posted.
#[rstest]
#[case::app_in_background(bring_other_app_to_front, MouseButton::Left, true)]
#[case::key_window(leave_in_front, MouseButton::Left, false)]
#[case::other_window_of_the_frontmost_app(focus_other_window, MouseButton::Left, false)]
#[case::right_click(bring_other_app_to_front, MouseButton::Right, false)]
#[tokio::test]
async fn background_left_clicks_press_the_element_under_the_point(
    target: ComputerTarget,
    #[case] change: fn(&mut Fake, &ComputerTarget),
    #[case] button: MouseButton,
    #[case] presses: bool,
    #[values(true, false)] pressable: bool,
) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.pressable = pressable;
    change(&mut fake, &target);
    let fake = Arc::new(fake);
    let action = ComputerAction::Click {
        x: 10.0,
        y: 10.0,
        button,
    };
    assert_eq!(
        run(&fake, &target, &action).await.unwrap(),
        ComputerResult::Posted
    );
    let clicked = [InputEvent::Click {
        x: 10.0,
        y: 10.0,
        button,
    }];
    let (pressed, posted): (&[Vec<u16>], &[InputEvent]) = if presses && pressable {
        (&[vec![0, 2]], &[])
    } else {
        (&[], &clicked)
    };
    assert_eq!(fake.pressed(), pressed);
    assert_eq!(fake.posted(), posted);
}

/// An element's own action goes to the element, so it needs neither focus nor an uncovered point.
#[rstest]
#[tokio::test]
async fn element_actions_need_no_focus(
    target: ComputerTarget,
    #[values(InputRoute::Window, InputRoute::Shared)] route: InputRoute,
) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.route = Some(route);
    fake.pressable = true;
    focus_nothing(&mut fake, &target);
    cover_point(&mut fake, &target);
    let fake = Arc::new(fake);
    assert_eq!(
        run(&fake, &target, &press()).await.unwrap(),
        ComputerResult::Pressed {
            method: PressMethod::Action
        }
    );
    assert_eq!(fake.pressed(), [vec![0, 2]]);
    assert_eq!(fake.posted(), []);
}

#[rstest]
#[tokio::test]
async fn action_only_press_never_falls_back_to_pointer_input(
    target: ComputerTarget,
    #[values(false, true)] pressable: bool,
) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.pressable = pressable;
    let fake = Arc::new(fake);
    let action: ComputerAction = serde_json::from_value(serde_json::json!({
        "action": "press", "element": button(), "allow_click": false,
    }))
    .unwrap();
    let result = run(&fake, &target, &action).await;
    if pressable {
        assert_eq!(
            result.unwrap(),
            ComputerResult::Pressed {
                method: PressMethod::Action
            }
        );
    } else {
        assert!(matches!(result, Err(ComputerError::InvalidAction(_))));
    }
    assert_eq!(fake.posted(), []);
}

/// Apps that draw their own controls often have no press action; those elements get a click at
/// their center instead, and the result says which happened.
#[rstest]
#[case::own_action(true, PressMethod::Action)]
#[case::click_fallback(false, PressMethod::Click)]
#[tokio::test]
async fn press_uses_the_element_action_or_clicks_its_center(
    target: ComputerTarget,
    #[case] pressable: bool,
    #[case] method: PressMethod,
) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.pressable = pressable;
    let fake = Arc::new(fake);
    let result = run(&fake, &target, &press()).await.unwrap();
    assert_eq!(result, ComputerResult::Pressed { method });
    let clicked = [InputEvent::Click {
        x: 10.0,
        y: 10.0,
        button: MouseButton::Left,
    }];
    let (pressed, posted): (&[Vec<u16>], &[InputEvent]) = if pressable {
        (&[vec![0, 2]], &[])
    } else {
        (&[], &clicked)
    };
    assert_eq!(fake.pressed(), pressed);
    assert_eq!(fake.posted(), posted);
}

const fn move_element(element: &mut ComputerElement) {
    element.frame.x = 4.0;
}
fn rename_element(element: &mut ComputerElement) {
    element.name = Some("Forward".into());
}
fn change_element_identifier(element: &mut ComputerElement) {
    element.identifier = Some("different-control".into());
}
fn replace_element(element: &mut ComputerElement) {
    element.path = vec![0, 3];
}
fn disable_element(element: &mut ComputerElement) {
    element.states.push(ElementState::Disabled);
}

/// A press acts only on the element as it was listed; anything else touches nothing.
#[rstest]
#[case::moved(move_element)]
#[case::renamed(rename_element)]
#[case::reidentified(change_element_identifier)]
#[case::gone(replace_element)]
#[case::disabled(disable_element)]
#[tokio::test]
async fn press_refuses_an_element_that_changed(
    target: ComputerTarget,
    #[case] change: fn(&mut ComputerElement),
    #[values(true, false)] pressable: bool,
) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.elements.iter_mut().for_each(change);
    fake.pressable = pressable;
    let fake = Arc::new(fake);
    let result = run(&fake, &target, &press()).await;
    assert!(
        matches!(
            result,
            Err(ComputerError::StaleElement | ComputerError::InvalidAction(_))
        ),
        "{result:?}"
    );
    assert_eq!(fake.pressed(), Vec::<Vec<u16>>::new());
    assert_eq!(fake.posted(), []);
}

/// Listing reads the window without input, so it needs no focus, only Accessibility.
#[rstest]
#[tokio::test]
async fn elements_need_accessibility_but_not_focus(target: ComputerTarget) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.focus = None;
    let fake = Arc::new(fake);
    assert_eq!(
        run(&fake, &target, &ComputerAction::Elements)
            .await
            .unwrap(),
        ComputerResult::Elements {
            elements: vec![button()],
            omitted: false,
            target: Box::new(target.clone()),
        }
    );

    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.accessibility = Some(PermissionStatus::NotGranted);
    let fake = Arc::new(fake);
    assert!(matches!(
        run(&fake, &target, &ComputerAction::Elements).await,
        Err(ComputerError::PermissionDenied(_))
    ));
}

#[rstest]
#[tokio::test]
async fn transparent_windows_do_not_cover_points(target: ComputerTarget) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.route = Some(InputRoute::Shared);
    fake.windows.insert(
        0,
        NativeWindow {
            id: 7,
            bounds: target.bounds.clone(),
            visible: false,
            app: None,
        },
    );
    let fake = Arc::new(fake);
    assert!(run(&fake, &target, &click()).await.is_ok());
}

#[rstest]
#[tokio::test]
async fn capture_needs_screen_recording_but_not_accessibility(target: ComputerTarget) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.accessibility = Some(PermissionStatus::NotGranted);
    fake.focus = None;
    let fake = Arc::new(fake);
    assert!(run(&fake, &target, &ComputerAction::Snapshot).await.is_ok());

    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.recording = Some(PermissionStatus::NotGranted);
    let fake = Arc::new(fake);
    assert!(matches!(
        run(&fake, &target, &ComputerAction::Snapshot).await,
        Err(ComputerError::PermissionDenied(_))
    ));
}

#[rstest]
#[tokio::test]
async fn focus_raises_the_window_and_confirms_focus(target: ComputerTarget) {
    let fake = Fake::focused(&target);
    assert!(run(&fake, &target, &ComputerAction::Focus).await.is_ok());
    assert_eq!(*fake.raised.lock().unwrap(), [target.window_id]);
}

fn decode(png_base64: &str) -> Result<(u32, u32, Vec<u8>), Box<dyn std::error::Error>> {
    let png = STANDARD.decode(png_base64)?;
    let mut reader = png::Decoder::new(Cursor::new(png)).read_info()?;
    let mut pixels = vec![0; reader.output_buffer_size().ok_or("unbounded PNG")?];
    let info = reader.next_frame(&mut pixels)?;
    pixels.truncate(info.buffer_size());
    Ok((info.width, info.height, pixels))
}

proptest! {
    /// A region capture returns only whole pixels inside the requested rectangle, and reports
    /// the desktop rectangle those pixels cover.
    #[test]
    fn region_captures_stay_inside_the_requested_rectangle(
        x in 0.0..140.0_f64,
        y in 0.0..100.0_f64,
        width in 1.0..20.0_f64,
        height in 1.0..20.0_f64,
    ) {
        // A small window keeps each case cheap; the crop rules do not depend on its size.
        let mut target = target();
        target.bounds.width = 160.0;
        target.bounds.height = 120.0;
        let fake = Fake::focused(&target);
        let rect = DisplayBounds { x, y, width, height };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let result = runtime.block_on(run(&fake, &target, &ComputerAction::SnapshotRegion { rect: rect.clone() }));
        let Ok(ComputerResult::Snapshot { png_base64, region: Some(actual), pixel_width, pixel_height, .. }) = result else {
            // Regions narrower than one whole pixel have nothing to return.
            prop_assert!(matches!(result, Err(ComputerError::InvalidAction(_))), "{result:?}");
            return Ok(());
        };
        prop_assert!(actual.x >= rect.x && actual.y >= rect.y);
        prop_assert!(actual.x + actual.width <= rect.x + rect.width + 1e-9);
        prop_assert!(actual.y + actual.height <= rect.y + rect.height + 1e-9);
        let (decoded_width, decoded_height, pixels) =
            decode(&png_base64).map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!((decoded_width, decoded_height), (pixel_width, pixel_height));
        // The first returned pixel is the window pixel at the snapped top-left corner.
        let corner = |edge: f64| (0..160_u32).find(|pixel| (f64::from(*pixel) - edge).abs() < 1e-9);
        let (column, row) = (corner(actual.x), corner(actual.y));
        prop_assert!(column.is_some() && row.is_some(), "{actual:?} is not on a pixel edge");
        let expected = pixel(column.unwrap_or_default(), row.unwrap_or_default());
        prop_assert_eq!(pixels.get(..4), Some(&expected[..]));
    }

    /// Text is split for delivery without losing or splitting characters.
    #[test]
    fn text_pieces_preserve_text_within_the_event_limit(text in "\\PC{1,200}") {
        let pieces = text_pieces(&text);
        prop_assert_eq!(pieces.concat(), text);
        prop_assert!(pieces.iter().all(|piece| !piece.is_empty() && piece.encode_utf16().count() <= MAX_TEXT_UNITS.max(2)));
    }
}

// A stray 66x20 helper window in front of an app's real window must not become its target.
#[rstest]
#[case(66.0, 20.0)]
#[case(20.0, 400.0)]
#[tokio::test]
async fn targets_skip_windows_too_small_to_work_in(
    target: ComputerTarget,
    #[case] width: f64,
    #[case] height: f64,
) {
    let mut stray = target.clone();
    stray.window_id = 7;
    stray.bounds.width = width;
    stray.bounds.height = height;
    let fake = Arc::new(Fake {
        windows: vec![window(&stray), window(&target)],
        ..Fake::default()
    });
    assert_eq!(fake.computer().targets().await.unwrap(), vec![target]);
}

#[rstest]
#[tokio::test]
async fn icons_return_pngs_only_for_known_apps(target: ComputerTarget) {
    let fake = Fake::focused(&target);
    let icons = fake
        .computer()
        .icons(&["dev.zed.Zed".to_owned(), "com.example.Missing".to_owned()])
        .await
        .unwrap();
    assert_eq!(icons.keys().collect::<Vec<_>>(), ["dev.zed.Zed"]);
    assert!(icons["dev.zed.Zed"].starts_with(&[137, 80, 78, 71]));
    let too_many: Vec<String> = (0..=bootty_computer::MAX_ICON_REQUEST)
        .map(|index| format!("com.example.app{index}"))
        .collect();
    assert!(fake.computer().icons(&too_many).await.is_err());
}

#[rstest]
#[case(InputRoute::Shared, "hello")]
#[case(InputRoute::Window, "")]
#[tokio::test]
async fn set_value_reaches_background_element_without_focus_or_events(
    target: ComputerTarget,
    #[case] route: InputRoute,
    #[case] value: &str,
) {
    let fake = Arc::new(Fake {
        windows: vec![window(&target)],
        route: Some(route),
        frontmost: Some(999),
        elements: vec![button()],
        ..Fake::default()
    });
    let result = fake
        .computer()
        .execute(
            &ComputerAccess::from_user_setting(true),
            &target,
            &ComputerAction::SetValue {
                element: Box::new(button()),
                value: value.into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result, ComputerResult::Posted);
    assert_eq!(
        *fake.values.lock().unwrap(),
        vec![(button().path, value.to_owned())]
    );
    assert_eq!(fake.posted(), Vec::<InputEvent>::new());
    assert!(fake.raised.lock().unwrap().is_empty());
}

#[rstest]
#[case(false)]
#[case(true)]
#[tokio::test]
async fn set_value_refuses_stale_or_disabled_elements(
    target: ComputerTarget,
    #[case] disabled: bool,
) {
    let mut current = button();
    if disabled {
        current.states.push(ElementState::Disabled);
    } else {
        current.identifier = Some("replacement".into());
    }
    let fake = Arc::new(Fake {
        windows: vec![window(&target)],
        elements: vec![current],
        ..Fake::default()
    });
    let result = fake
        .computer()
        .execute(
            &ComputerAccess::from_user_setting(true),
            &target,
            &ComputerAction::SetValue {
                element: Box::new(button()),
                value: "text".into(),
            },
        )
        .await;
    if disabled {
        assert!(matches!(result, Err(ComputerError::InvalidAction(_))));
    } else {
        assert!(matches!(result, Err(ComputerError::StaleElement)));
    }
    assert!(fake.values.lock().unwrap().is_empty());
    assert_eq!(fake.posted(), Vec::<InputEvent>::new());
}

#[rstest]
#[case(ComputerAction::ButtonDown { x: 10.0, y: 20.0, button: MouseButton::Left })]
#[case(ComputerAction::ButtonUp { x: 10.0, y: 20.0, button: MouseButton::Right })]
#[case(ComputerAction::KeyDown { key: Key::Control })]
#[case(ComputerAction::KeyUp { key: Key::Right })]
#[tokio::test]
async fn independent_input_requires_the_backend_capability(
    target: ComputerTarget,
    #[case] action: ComputerAction,
) {
    let fake = Fake::focused(&target);
    assert!(matches!(
        run(&fake, &target, &action).await,
        Err(ComputerError::Unsupported)
    ));
    assert_eq!(fake.posted(), []);
}

#[rstest]
#[case("backend")]
#[case("display")]
#[case("launch")]
#[case("pid")]
#[tokio::test]
async fn backend_and_display_are_part_of_exact_window_identity(
    mut target: ComputerTarget,
    #[case] changed: &str,
) {
    target.backend_id = Some("x11:owned-connection".into());
    target.display = Some(":183".into());
    let fake = Fake::focused(&target);
    match changed {
        "backend" => target.backend_id = Some("x11:replacement-connection".into()),
        "display" => target.display = Some(":184".into()),
        "launch" => target.launch_time += 1.0,
        _ => target.process_id = target.process_id.saturating_add(1),
    }
    assert!(matches!(
        run(&fake, &target, &ComputerAction::Snapshot).await,
        Err(ComputerError::StaleTarget)
    ));
    assert_eq!(fake.posted(), []);
}

#[rstest]
#[tokio::test]
async fn cancelled_native_worker_releases_even_when_post_finishes_late(target: ComputerTarget) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    let (entered, wait_entered) = std::sync::mpsc::channel();
    let (resume, wait_resume) = std::sync::mpsc::channel();
    let (released, wait_released) = std::sync::mpsc::channel();
    fake.independent = true;
    fake.entered = Some(entered);
    fake.resume = Mutex::new(Some(wait_resume));
    fake.released = Some(released);
    let fake = Arc::new(fake);
    let computer = fake.computer();
    let worker = computer.clone();
    let task = tokio::spawn(async move {
        worker
            .execute(
                &ComputerAccess::from_user_setting(true),
                &target,
                &ComputerAction::KeyDown { key: Key::Control },
            )
            .await
    });
    tokio::task::spawn_blocking(move || wait_entered.recv().unwrap())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    resume.send(()).unwrap();
    tokio::task::spawn_blocking(move || wait_released.recv().unwrap())
        .await
        .unwrap();
    let trace = fake.trace.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(trace.first(), Some(&"posted"));
    assert!(trace.iter().skip(1).all(|entry| *entry == "released"));
    assert!(trace.len() >= 2);
    drop(trace);
}

#[rstest]
#[tokio::test]
async fn dropping_the_native_owner_releases_its_completed_hold(target: ComputerTarget) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    let (released, wait_released) = std::sync::mpsc::channel();
    fake.independent = true;
    fake.released = Some(released);
    let fake = Arc::new(fake);
    let computer = fake.computer();
    computer
        .execute(
            &ComputerAccess::from_user_setting(true),
            &target,
            &ComputerAction::ButtonDown {
                x: 10.0,
                y: 20.0,
                button: MouseButton::Left,
            },
        )
        .await
        .unwrap();
    drop(computer);
    tokio::task::spawn_blocking(move || wait_released.recv().unwrap())
        .await
        .unwrap();
    assert_eq!(
        *fake.trace.lock().unwrap_or_else(PoisonError::into_inner),
        ["posted", "released"]
    );
}

#[rstest]
#[tokio::test]
async fn awaited_shutdown_releases_input_and_rejects_new_holds(target: ComputerTarget) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    fake.independent = true;
    let fake = Arc::new(fake);
    let computer = fake.computer();
    computer
        .execute(
            &ComputerAccess::from_user_setting(true),
            &target,
            &ComputerAction::KeyDown { key: Key::Control },
        )
        .await
        .unwrap();
    computer.shutdown().await.unwrap();
    assert_eq!(
        *fake.trace.lock().unwrap_or_else(PoisonError::into_inner),
        ["posted", "released"]
    );
    assert!(matches!(
        computer
            .execute(
                &ComputerAccess::from_user_setting(true),
                &target,
                &ComputerAction::KeyDown { key: Key::Right }
            )
            .await,
        Err(ComputerError::TargetUnavailable)
    ));
    assert_eq!(fake.posted(), [InputEvent::KeyDown { key: Key::Control }]);
}
