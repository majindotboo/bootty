//! Bootty font policy through GPUI's public platform interface.
use anyhow::Result;
use futures::channel::oneshot;
use gpui_kit::{
    Action, AnyWindowHandle, AppLifecyclePhase, BackgroundExecutor, ClipboardItem,
    ClipboardReadError, CursorStyle, ForegroundExecutor, Keymap, Menu, MenuItem, OwnedMenu,
    PathPromptOptions, Platform, PlatformDisplay, PlatformGestures, PlatformKeyboardLayout,
    PlatformKeyboardMapper, PlatformTextSystem, PlatformWindow, ScreenCaptureSource,
    SystemNotification, SystemNotificationResponse, Task, ThermalState, WindowAppearance,
    WindowButtonLayout, WindowParams,
};
use smallvec::SmallVec;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

// Forward the platform contract so native behavior stays owned by GPUI.
macro_rules! forward {
    () => {};
    ($(#[$meta:meta])* fn $name:ident (&self $(, $arg:ident : $ty:ty)* $(,)?) -> $ret:ty; $($rest:tt)*) => {
        $(#[$meta])* fn $name(&self, $($arg: $ty),*) -> $ret {
            self.inner.$name($($arg),*)
        }
        forward! { $($rest)* }
    };
    ($(#[$meta:meta])* fn $name:ident (&mut self $(, $arg:ident : $ty:ty)* $(,)?) -> $ret:ty; $($rest:tt)*) => {
        $(#[$meta])* fn $name(&mut self, $($arg: $ty),*) -> $ret {
            self.inner.$name($($arg),*)
        }
        forward! { $($rest)* }
    };
}

pub struct BoottyPlatform {
    inner: Rc<dyn Platform>,
    text_system: Arc<dyn PlatformTextSystem>,
}
impl BoottyPlatform {
    pub(crate) fn new(inner: Rc<dyn Platform>, text_system: Arc<dyn PlatformTextSystem>) -> Self {
        Self { inner, text_system }
    }
}
impl Platform for BoottyPlatform {
    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.text_system.clone()
    }
    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> Result<Box<dyn PlatformWindow>> {
        self.inner.open_window(handle, options)
    }
    forward! {
        fn background_executor(&self) -> BackgroundExecutor;
        fn foreground_executor(&self) -> ForegroundExecutor;
        fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) -> ();
        fn quit(&self) -> ();
        fn restart(&self, binary_path: Option<PathBuf>, arguments: Vec<OsString>) -> ();
        fn activate(&self, ignoring_other_apps: bool) -> ();
        fn hide(&self) -> ();
        fn hide_other_apps(&self) -> ();
        fn unhide_other_apps(&self) -> ();
        fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>>;
        fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>>;
        fn active_window(&self) -> Option<AnyWindowHandle>;
        fn window_stack(&self) -> Option<Vec<AnyWindowHandle>>;
        fn is_screen_capture_supported(&self) -> bool;
        fn screen_capture_sources( &self, ) -> oneshot::Receiver<anyhow::Result<Vec<Rc<dyn ScreenCaptureSource>>>>;
        fn window_appearance(&self) -> WindowAppearance;
        fn set_window_appearance(&self, appearance: Option<WindowAppearance>) -> ();
        fn button_layout(&self) -> Option<WindowButtonLayout>;
        fn open_url(&self, url: &str) -> ();
        fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) -> ();
        fn register_url_scheme(&self, url: &str) -> Task<Result<()>>;
        fn prompt_for_paths( &self, options: PathPromptOptions, ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>>;
        fn prompt_for_new_path( &self, directory: &Path, suggested_name: Option<&str>, ) -> oneshot::Receiver<Result<Option<PathBuf>>>;
        fn can_select_mixed_files_and_dirs(&self) -> bool;
        fn reveal_path(&self, path: &Path) -> ();
        fn open_with_system(&self, path: &Path) -> ();
        fn on_quit(&self, callback: Box<dyn FnMut() -> bool>) -> ();
        fn on_reopen(&self, callback: Box<dyn FnMut()>) -> ();
        fn on_system_wake(&self, callback: Box<dyn FnMut()>) -> ();
        fn on_app_lifecycle(&self, callback: Box<dyn FnMut(AppLifecyclePhase)>) -> ();
        fn on_memory_warning(&self, callback: Box<dyn FnMut()>) -> ();
        fn gestures(&self) -> Option<Rc<dyn PlatformGestures>>;
        fn set_menus(&self, menus: Vec<Menu>, keymap: &Keymap) -> ();
        fn get_menus(&self) -> Option<Vec<OwnedMenu>>;
        fn set_dock_menu(&self, menu: Vec<MenuItem>, keymap: &Keymap) -> ();
        fn perform_dock_menu_action(&self, action: usize) -> ();
        fn add_recent_document(&self, path: &Path) -> ();
        fn update_jump_list(&self, menus: Vec<MenuItem>, entries: Vec<SmallVec<[PathBuf; 2]>>) -> Task<Vec<SmallVec<[PathBuf; 2]>>>;
        fn on_app_menu_action(&self, callback: Box<dyn FnMut(&dyn Action)>) -> ();
        fn on_will_open_app_menu(&self, callback: Box<dyn FnMut()>) -> ();
        fn on_validate_app_menu_command(&self, callback: Box<dyn FnMut(&dyn Action) -> bool>) -> ();
        fn thermal_state(&self) -> ThermalState;
        fn on_thermal_state_change(&self, callback: Box<dyn FnMut()>) -> ();
        fn set_app_identity(&self, identifier: &str, name: &str) -> ();
        fn show_system_notification(&self, notification: SystemNotification) -> ();
        fn dismiss_system_notification(&self, tag: &str) -> ();
        fn on_system_notification_response( &self, callback: Box<dyn FnMut(SystemNotificationResponse)>, ) -> ();
        fn compositor_name(&self) -> &'static str;
        fn app_path(&self) -> Result<PathBuf>;
        fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf>;
        fn set_cursor_style(&self, style: CursorStyle) -> ();
        fn hide_cursor_until_mouse_moves(&self) -> ();
        fn is_cursor_visible(&self) -> bool;
        fn should_auto_hide_scrollbars(&self) -> bool;
        fn read_from_clipboard(&self) -> Option<ClipboardItem>;
        fn write_to_clipboard(&self, item: ClipboardItem) -> ();
        fn read_from_clipboard_async(&self) -> Task<Result<Option<ClipboardItem>, ClipboardReadError>>;
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        fn read_from_primary(&self) -> Option<ClipboardItem>;
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        fn write_to_primary(&self, item: ClipboardItem) -> ();
        #[cfg(target_os = "macos")]
        fn read_from_find_pasteboard(&self) -> Option<ClipboardItem>;
        #[cfg(target_os = "macos")]
        fn write_to_find_pasteboard(&self, item: ClipboardItem) -> ();
        fn write_credentials(&self, url: &str, username: &str, password: &[u8]) -> Task<Result<()>>;
        fn read_credentials(&self, url: &str) -> Task<Result<Option<(String, Vec<u8>)>>>;
        fn delete_credentials(&self, url: &str) -> Task<Result<()>>;
        fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout>;
        fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper>;
        fn on_keyboard_layout_change(&self, callback: Box<dyn FnMut()>) -> ();
    }
}
