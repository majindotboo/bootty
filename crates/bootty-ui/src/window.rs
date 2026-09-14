#[cfg(target_os = "macos")]
use num_traits::ToPrimitive as _;
#[cfg(target_os = "macos")]
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

#[cfg(target_os = "macos")]
use objc2::runtime::NSObjectProtocol;
#[cfg(target_os = "macos")]
use objc2::{MainThreadMarker, sel};
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplication, NSScreen, NSTitlebarSeparatorStyle, NSWindow};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSNumber, NSString};

/// Notch geometry for one concrete display, in screen points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MacosScreenFacts {
    pub notched: bool,
    pub notch_height: f32,
    pub notch_span: Option<(f32, f32)>,
}

#[cfg(target_os = "macos")]
fn active_window(app: &NSApplication) -> Option<objc2::rc::Retained<NSWindow>> {
    app.keyWindow()
        .or_else(|| app.mainWindow())
        .or_else(|| app.windows().firstObject())
}

#[cfg(target_os = "macos")]
fn with_active_window(action: impl FnOnce(&NSWindow)) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if let Some(window) = active_window(&NSApplication::sharedApplication(mtm)) {
        action(&window);
    }
}

/// Whether the active window's screen has a camera-housing notch.
///
/// Detected by display name (the
/// built-in Liquid Retina panel on 2021+ Macs) because `safeAreaInsets`/`auxiliaryTopLeftArea` zero
/// out when the menu bar is hidden in fullscreen. Mirrors wezterm's detection.
#[must_use]
pub fn macos_active_screen_is_notched() -> bool {
    macos_screen_facts(None).notched
}

/// Resolve notch geometry for the requested Core Graphics display id. `None` retains the legacy
/// active-window behavior for callers that do not own a concrete window.
#[must_use]
pub fn macos_screen_facts(display_id: Option<u32>) -> MacosScreenFacts {
    platform_screen_facts(display_id)
}

#[cfg(target_os = "macos")]
fn name_reads_as_notched(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("built-in") || name.contains("builtin") || name.contains("liquid retina")
}

#[cfg(target_os = "macos")]
fn platform_screen_facts(display_id: Option<u32>) -> MacosScreenFacts {
    let Some(mtm) = MainThreadMarker::new() else {
        return MacosScreenFacts::default();
    };
    let app = NSApplication::sharedApplication(mtm);
    let screen = display_id.map_or_else(
        || active_window(&app).and_then(|window| window.screen()),
        |id| screen_for_display_id(mtm, id),
    );
    screen
        .as_deref()
        .map_or_else(MacosScreenFacts::default, |screen| {
            screen_facts(screen, display_id.or_else(|| screen_display_id(screen)))
        })
}

#[cfg(not(target_os = "macos"))]
fn platform_screen_facts(_display_id: Option<u32>) -> MacosScreenFacts {
    MacosScreenFacts::default()
}

/// Raw height of the active window's camera-housing/menu-bar exclusion band, in points.
///
/// Returns
/// `0.0` off macOS or when it can't be measured. The layout layer calibrates this value to the
/// physical notch-clear line.
#[must_use]
pub fn macos_active_screen_notch_height() -> f32 {
    macos_screen_facts(None).notch_height
}

#[cfg(target_os = "macos")]
static CACHED_NOTCH_HEIGHTS: LazyLock<Mutex<HashMap<u32, u32>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(target_os = "macos")]
fn measure_screen_notch_height(screen: &NSScreen) -> f32 {
    // auxiliaryTopLeftArea is Apple's API for laying out around the camera housing, so it stays
    // valid in fullscreen with the menu bar hidden (where safeAreaInsets zeroes out). Its band can
    // track the menu-bar exclusion line, which is slightly lower than the physical notch.
    if screen.respondsToSelector(sel!(auxiliaryTopLeftArea)) {
        let height = screen
            .auxiliaryTopLeftArea()
            .size
            .height
            .to_f32()
            .unwrap_or_default();
        if height > 0.0 {
            return height;
        }
    }
    if screen.respondsToSelector(sel!(safeAreaInsets)) {
        return screen.safeAreaInsets().top.to_f32().unwrap_or_default();
    }
    0.0
}

/// Horizontal span of the active screen's camera housing in window points from the left screen
/// edge. Returns `None` off macOS or when the notched display geometry can't be inferred.
#[must_use]
pub fn macos_active_screen_notch_span() -> Option<(f32, f32)> {
    macos_screen_facts(None).notch_span
}

#[cfg(target_os = "macos")]
fn screen_facts(screen: &NSScreen, display_id: Option<u32>) -> MacosScreenFacts {
    let frame = screen.frame();
    let Some(width) = frame
        .size
        .width
        .to_f32()
        .filter(|width| width.is_finite() && *width > 0.0)
    else {
        return MacosScreenFacts::default();
    };

    let named_as_notched = name_reads_as_notched(&screen.localizedName().to_string());
    let measured_height = measure_screen_notch_height(screen);
    let notch_height = if measured_height > 0.0 {
        if let Some(display_id) = display_id {
            CACHED_NOTCH_HEIGHTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(display_id, measured_height.to_bits());
        }
        measured_height
    } else if named_as_notched {
        display_id
            .and_then(|display_id| {
                CACHED_NOTCH_HEIGHTS
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&display_id)
                    .copied()
            })
            .map_or(0.0, f32::from_bits)
    } else {
        0.0
    };

    let mut notch_span = None;
    if screen.respondsToSelector(sel!(auxiliaryTopLeftArea))
        && screen.respondsToSelector(sel!(auxiliaryTopRightArea))
    {
        let left = screen.auxiliaryTopLeftArea();
        let right = screen.auxiliaryTopRightArea();
        let span = (left.origin.x + left.size.width - frame.origin.x)
            .to_f32()
            .zip((right.origin.x - frame.origin.x).to_f32());
        if let Some((notch_left, notch_right)) = span
            && left.size.height > 0.0
            && right.size.height > 0.0
            && notch_right > notch_left
        {
            notch_span = Some((notch_left.max(0.0), notch_right.min(width)));
        }
    }

    if notch_span.is_none() && named_as_notched {
        let fallback_width = 220.0_f32.min(width * 0.35);
        let center = width * 0.5;
        notch_span = Some((
            fallback_width.mul_add(-0.5, center),
            fallback_width.mul_add(0.5, center),
        ));
    }

    MacosScreenFacts {
        notched: named_as_notched || measured_height > 0.0,
        notch_height,
        notch_span,
    }
}

#[cfg(target_os = "macos")]
fn screen_for_display_id(
    mtm: MainThreadMarker,
    display_id: u32,
) -> Option<objc2::rc::Retained<NSScreen>> {
    let screens = NSScreen::screens(mtm);
    (0..screens.count())
        .map(|index| screens.objectAtIndex(index))
        .find(|screen| screen_display_id(screen) == Some(display_id))
}

#[cfg(target_os = "macos")]
fn screen_display_id(screen: &NSScreen) -> Option<u32> {
    let key = NSString::from_str("NSScreenNumber");
    let value = screen.deviceDescription().objectForKey(&key)?;
    let number = value.downcast::<NSNumber>().ok()?;
    u32::try_from(number.unsignedIntegerValue()).ok()
}

/// Toggle the drop shadow of the window with a unique title.
#[cfg(target_os = "macos")]
pub fn macos_set_window_shadow(title: &str, enabled: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let windows = NSApplication::sharedApplication(mtm).windows();
    // GPUI exposes the window title without unsafe native-handle casts. Fail closed if it is
    // ambiguous; never redirect a workspace mutation to the currently focused auxiliary window.
    let matching = (0..windows.count())
        .map(|ix| windows.objectAtIndex(ix))
        .filter(|window| window.title().to_string() == title)
        .collect::<Vec<_>>();
    if let [window] = matching.as_slice()
        && window.hasShadow() != enabled
    {
        window.setHasShadow(enabled);
        window.invalidateShadow();
    }
}

#[cfg(not(target_os = "macos"))]
pub const fn macos_set_window_shadow(_title: &str, _enabled: bool) {}

/// Remove the separator under the transparent macOS titlebar.
#[cfg(target_os = "macos")]
pub fn macos_disable_titlebar_separator() {
    with_active_window(|window| {
        if window.titlebarSeparatorStyle() != NSTitlebarSeparatorStyle::None {
            window.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub const fn macos_disable_titlebar_separator() {}

// macOS automatic window tabbing claims Cmd+T (newWindowForTab:) at the OS level before it reaches
// the app, which would shadow Bootty's new-tab shortcut. Opt out so the key reaches us. Must run
// before any window is created, since the class flag is read at window-creation time.
#[cfg(target_os = "macos")]
pub fn disable_automatic_window_tabbing() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);
    }
}

#[cfg(not(target_os = "macos"))]
pub const fn disable_automatic_window_tabbing() {}

/// Whether macOS starts non-native fullscreen windowed so GPUI can apply simple fullscreen after
/// the concrete window exists.
#[must_use]
pub const fn handles_macos_non_native_fullscreen_frame() -> bool {
    cfg!(target_os = "macos")
}
