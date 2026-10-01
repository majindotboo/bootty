use std::{cell::RefCell, io::Read};

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, AppContext, AsyncApp, WeakEntity, WindowOptions, px};

use crate::{Connection, WorkspaceView};

// The upstream embedding bridge supports one view for the app's lifetime.
// UIKit invokes these entry points only on the main thread.
thread_local! {
    static VIEW: RefCell<Option<(WeakEntity<WorkspaceView>, AsyncApp)>> = const { RefCell::new(None) };
}

// Only the exported symbol attribute needs a lint exception. No pointer ABI or
// unsafe memory operations are introduced in Bootty.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn bootty_mobile_register() {
    gpui_mobile::ios::ffi::set_app_callback(Box::new(|cx: &mut App| {
        gpui_kit::init(cx);
        match gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| WorkspaceView::new(window, cx))
        }) {
            Ok((_, view)) => {
                VIEW.with(|slot| slot.replace(Some((view.downgrade(), cx.to_async()))))
            }
            Err(error) => {
                eprintln!("Couldn’t open Bootty mobile: {error}");
                None
            }
        };
    }));
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn bootty_mobile_reload() {
    VIEW.with(|slot| {
        if let Some((view, cx)) = slot.borrow().as_ref() {
            cx.update(|cx| {
                let _ = view.update(cx, |this, cx| {
                    // Replacing the task cancels an older connection before it can publish.
                    this.connection_task = Some(cx.spawn(async |view, cx| {
                        let connection = cx
                            .background_executor()
                            .spawn(async {
                                let path = std::env::temp_dir().join("bootty-pairing.txt");
                                let file = std::fs::File::open(&path)
                                    .map_err(|error| error.to_string())?;
                                let mut bytes = Vec::new();
                                let read = file.take(8193).read_to_end(&mut bytes);
                                let _ = std::fs::remove_file(path);
                                read.map_err(|error| error.to_string())?;
                                let code = std::str::from_utf8(&bytes)
                                    .map_err(|_| "Invalid pairing code")?;
                                Connection::from_code(code)
                            })
                            .await;
                        let _ = view.update(cx, |this, cx| this.connect(connection, cx));
                    }));
                });
            });
        }
    });
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn bootty_mobile_appearance(dark: bool, text_size: f32) {
    if !text_size.is_finite() || text_size <= 0.0 {
        return;
    }
    VIEW.with(|slot| {
        if let Some((_, cx)) = slot.borrow().as_ref() {
            cx.update(|cx| {
                Theme::change(
                    if dark {
                        ThemeMode::Dark
                    } else {
                        ThemeMode::Light
                    },
                    None,
                    cx,
                );
                Theme::update(cx, |theme| theme.font_size = px(text_size));
            });
        }
    });
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn bootty_mobile_active(active: bool) {
    VIEW.with(|slot| {
        if let Some((view, cx)) = slot.borrow().as_ref() {
            cx.update(|cx| {
                let _ = view.update(cx, |this, cx| {
                    this.set_active(active, cx);
                });
            });
        }
    });
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn bootty_mobile_disconnect() {
    VIEW.with(|slot| {
        if let Some((view, cx)) = slot.borrow().as_ref() {
            cx.update(|cx| {
                let _ = view.update(cx, |this, cx| {
                    this.connection_task = None;
                    this.connect(Err("Disconnected".into()), cx);
                });
            });
        }
    });
}
