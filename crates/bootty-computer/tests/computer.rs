use std::{
    io::Cursor,
    sync::{Arc, Mutex, PoisonError},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_computer::{
    Computer, ComputerAccess, ComputerAction, ComputerError, ComputerResult, ComputerStatus,
    ComputerTarget, DisplayBounds, HostCaptureRegion, MouseButton, PermissionStatus,
    control::{MAX_TEXT_UNITS, text_pieces},
    desktop::{Desktop, Focus, Image, InputEvent, NativeWindow},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};

#[fixture]
fn target() -> ComputerTarget {
    ComputerTarget {
        window_id: 42,
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
    windows: Vec<NativeWindow>,
    focus: Option<Focus>,
    accessibility: Option<PermissionStatus>,
    recording: Option<PermissionStatus>,
    secure: bool,
    posted: Mutex<Vec<InputEvent>>,
    raised: Mutex<Vec<u32>>,
}

impl Fake {
    /// The target is the only window, focused and in front.
    fn focused(target: &ComputerTarget) -> Arc<Self> {
        Arc::new(Self {
            windows: vec![window(target)],
            focus: Some(Focus {
                process_id: target.process_id,
                window: Some(target.bounds.clone()),
            }),
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

    fn focus(&self) -> Option<Focus> {
        self.focus.clone()
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
        self.posted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event.clone());
        Ok(())
    }

    fn raise(&self, window: &NativeWindow) -> Result<(), ComputerError> {
        self.raised
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(window.id);
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
async fn focused_uncovered_input_reaches_the_window(target: ComputerTarget) {
    let fake = Fake::focused(&target);
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
    if let Some(focus) = fake.focus.as_mut() {
        focus.process_id = 999;
    }
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

/// Each case changes one fact about the desktop; input must refuse and post nothing.
#[rstest]
#[case::window_closed(close)]
#[case::window_moved(move_window)]
#[case::process_relaunched(relaunch)]
#[case::other_app_focused(focus_other_app)]
#[case::point_covered(cover_point)]
#[case::two_windows_at_the_same_frame(duplicate_frame)]
#[case::accessibility_missing(revoke_accessibility)]
#[case::secure_input(secure_input)]
#[tokio::test]
async fn input_refuses_anything_but_the_exact_focused_window(
    target: ComputerTarget,
    #[case] change: fn(&mut Fake, &ComputerTarget),
) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
    change(&mut fake, &target);
    let fake = Arc::new(fake);
    let result = run(&fake, &target, &click()).await;
    assert!(result.is_err(), "input must be refused, got {result:?}");
    assert_eq!(fake.posted(), []);
}

#[rstest]
#[tokio::test]
async fn transparent_windows_do_not_cover_points(target: ComputerTarget) {
    let mut fake = Arc::into_inner(Fake::focused(&target)).unwrap_or_default();
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
