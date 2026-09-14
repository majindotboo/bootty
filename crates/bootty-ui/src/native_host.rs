use std::{cell::RefCell, rc::Rc, sync::Arc};

use crate::assets::BoottyAssets;
use anyhow::{Context as _, Result, anyhow};
use bootty_config::config::{BoottyConfig, OnLastWindowClosed};
use bootty_control::{ControlPlane, ControlServer};
#[cfg(target_os = "macos")]
use gpui_kit::KeyBinding;
use gpui_kit::component::Root;
use gpui_kit::{App, Entity};

use crate::gpui_workspace::GpuiWorkspace;

fn workspace_view(root: &Root) -> Result<Entity<GpuiWorkspace>> {
    root.view()
        .clone()
        .downcast::<GpuiWorkspace>()
        .map_err(|_| anyhow!("Bootty Root must own a GpuiWorkspace view"))
}

#[derive(Clone)]
struct ReopenContext {
    config: BoottyConfig,
    window_state_key: String,
    backends: Arc<bootty_mux::provider::MuxBackendRegistry>,
    control_plane: ControlPlane,
}

fn reopen_window(
    reopen_context: &Rc<RefCell<Option<ReopenContext>>>,
    reopen_control_server: &Rc<RefCell<Option<ControlServer>>>,
    cx: &mut App,
) {
    if let Some(window) = cx.windows().into_iter().next() {
        let _ = window.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let Some(reopen) = reopen_context.borrow().clone() else {
        return;
    };
    let state_key = reopen.window_state_key.clone();
    let control_state_key = state_key.clone();
    let control_plane = reopen.control_plane.clone();
    let server_control_plane = control_plane.clone();
    let config = reopen.config;
    let backends = reopen.backends;
    let window = match GpuiWorkspace::open(config, state_key, backends, control_plane, cx) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("reopen Bootty window: {error:#}");
            return;
        }
    };
    let _ = window.update(cx, |_, window, _| window.activate_window());
    let Ok(root) = window.read(cx) else {
        return;
    };
    let Ok(workspace) = workspace_view(root) else {
        return;
    };
    let (commands, catalog, _) = workspace.read(cx).control_binding();
    reopen_control_server.replace(None);
    if let Ok(server) =
        ControlServer::spawn(&control_state_key, commands, catalog, &server_control_plane)
    {
        reopen_control_server.replace(Some(server));
    }
}

/// Run the native application and its local control server.
///
/// # Errors
/// Returns errors from application, workspace, font, or control-server initialization.
pub fn run(
    config: BoottyConfig,
    window_state_key: String,
    backends: Arc<bootty_mux::provider::MuxBackendRegistry>,
) -> Result<()> {
    // The class flag is read at window creation. If automatic tabbing remains enabled, macOS
    // consumes Cmd+T as newWindowForTab: before GPUI can dispatch Bootty's key binding.
    crate::window::disable_automatic_window_tabbing();

    let launch_result = Rc::new(RefCell::new(None));
    let app_launch_result = Rc::clone(&launch_result);
    let control_server = Rc::new(RefCell::new(None));
    let app_control_server = Rc::clone(&control_server);
    let reopen_context = Rc::new(RefCell::new(None::<ReopenContext>));
    let app_reopen_context = Rc::clone(&reopen_context);
    let app_menu = Rc::new(RefCell::new(None));
    let launch_menu = Rc::clone(&app_menu);

    let platform = gpui_kit::platform::current_platform(false);
    let mappings = crate::font_mapping::FontMappings::default();
    let text_system =
        crate::font_mapping::wrap_text_system(platform.text_system(), mappings.clone());
    let terminal_text_system = crate::gpui::TerminalPlatformTextSystem(text_system.clone());
    let platform = Rc::new(crate::native_platform::BoottyPlatform::new(
        platform,
        text_system,
    ));
    let application = gpui_kit::Application::with_platform(platform).with_assets(BoottyAssets);
    let reopen_control_server = Rc::clone(&control_server);
    application.on_reopen(move |cx| reopen_window(&reopen_context, &reopen_control_server, cx));
    let (url_sender, url_receiver) = async_channel::unbounded();
    application.on_open_urls(move |urls| {
        let _ = url_sender.try_send(urls);
    });
    application.run(move |cx| {
        cx.set_global(mappings);
        cx.set_global(terminal_text_system);
        let result = launch(
            config,
            window_state_key,
            backends,
            &app_control_server,
            &launch_menu,
            &app_reopen_context,
            cx,
        );
        if result.is_err() {
            cx.quit();
        }
        app_launch_result.replace(Some(result));
        cx.spawn(async move |cx| {
            while let Ok(urls) = url_receiver.recv().await {
                cx.update(|cx| {
                    for handle in cx.windows() {
                        let Some(handle) = handle.downcast::<Root>() else {
                            continue;
                        };
                        let Ok(root) = handle.read(cx) else {
                            continue;
                        };
                        let Ok(workspace) = workspace_view(root) else {
                            continue;
                        };
                        let _ = handle.update(cx, |_, window, cx| {
                            for url in &urls {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.open_setting_url(url, window, cx);
                                });
                            }
                        });
                        break;
                    }
                });
            }
        })
        .detach();
    });

    launch_result
        .borrow_mut()
        .take()
        .unwrap_or_else(|| Err(anyhow!("GPUI exited before Bootty finished launching")))
}

fn launch(
    config: BoottyConfig,
    window_state_key: String,
    backends: Arc<bootty_mux::provider::MuxBackendRegistry>,
    control_server: &Rc<RefCell<Option<ControlServer>>>,
    app_menu: &Rc<RefCell<Option<crate::menu::AppMenu>>>,
    reopen_context: &Rc<RefCell<Option<ReopenContext>>>,
    cx: &mut App,
) -> Result<()> {
    let startup_variant = config
        .appearance
        .mode
        .variant(crate::theme::appearance_variant(cx.window_appearance()));
    BoottyAssets
        .load_fonts(cx.text_system())
        .context("load bundled Bootty fonts")?;
    crate::gpui::init_ui_theme(
        crate::theme::theme_from_config(&config, startup_variant),
        cx,
    );
    crate::gpui::update_ui_font(config.font.ui_families(), config.font.ui_size, cx);
    crate::gpui::update_ui_font_weights(&config.font.ui_weights, cx);
    let localizer = crate::i18n::Localizer::new(&config.locale)?;
    crate::i18n::publish(&localizer, cx);
    crate::gpui_document_panel::init(cx);
    app_menu.replace(crate::menu::install(&localizer));
    #[cfg(target_os = "macos")]
    cx.bind_keys([KeyBinding::new(
        "cmd-`",
        crate::gpui_actions::CycleApplicationWindow,
        None,
    )]);
    let quit_after_last_window =
        config.on_last_window_closed == OnLastWindowClosed::QuitApp || !cfg!(target_os = "macos");
    let reopen = ReopenContext {
        config: config.clone(),
        window_state_key: window_state_key.clone(),
        backends: Arc::clone(&backends),
        control_plane: ControlPlane::default(),
    };
    let control_state_key = window_state_key.clone();
    let control_plane = reopen.control_plane.clone();
    let workspace_control_plane = control_plane.clone();
    let window = GpuiWorkspace::open(
        config,
        window_state_key,
        backends,
        workspace_control_plane,
        cx,
    )
    .context("open Bootty window")?;
    cx.activate(true);
    window
        .update(cx, |_, window, _| window.activate_window())
        .context("activate Bootty window")?;

    let workspace = workspace_view(window.read(cx)?)?;
    let (commands, catalog, _workspace_control_plane) = workspace.read(cx).control_binding();
    control_server.replace(Some(ControlServer::spawn(
        &control_state_key,
        commands,
        catalog,
        &control_plane,
    )?));
    reopen_context.replace(Some(reopen));

    cx.on_window_closed(move |cx, _| {
        if quit_after_last_window && cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    Ok(())
}
