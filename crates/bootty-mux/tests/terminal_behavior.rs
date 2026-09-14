use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};

use anyhow::Result;
use bootty_config::config::{AppearanceVariant, BoottyConfig, MultiplexerBackendConfig};
use bootty_mux::terminal::TerminalRuntime;
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::MuxBackend,
    capability::BindingCapabilityDescriptor,
    command::MuxCommand,
    controller::SpaceId,
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider,
        MuxBackendRegistry, MuxCommandDispatch, PaneBehavior, PaneTopology, PersistedSessionPolicy,
        SelectionPublicationPolicy, TerminalProgressPolicy, TerminalResidency,
    },
    repository::{SpaceMuxOverride, SpaceRemoteOverride},
    snapshot::{MuxPaneAnchor, MuxSnapshot},
    terminal::{BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest},
    workspace::WorkspaceRuntime,
};
use bootty_terminal::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::{
    TerminalSessionConfig, frame_source::TerminalFrameSource, terminal_session::DrainStats,
};
use bootty_terminal::{
    terminal_engine::{
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
    },
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MouseInput},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::rstest;

#[rstest]
fn preparing_visible_windows_keeps_keyboard_focus_and_binding_identity() -> Result<()> {
    let starts = Arc::new(AtomicUsize::new(0));
    let provider = StaleCacheProvider::native(Arc::clone(&starts));
    let inputs = Arc::clone(&provider.inputs);
    let registry = Arc::new(MuxBackendRegistry::from_app_providers(
        [Arc::new(provider)],
        [MuxBackendKind::Native],
    )?);
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Native,
        ..Default::default()
    };
    let mut terminal = bootty_mux::terminal::ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        registry,
        &config,
        TerminalSessionConfig::default(),
        Arc::new(|| {}),
    )?;
    let scope = SpaceId::from_persistence(1);
    let other_scope = SpaceId::from_persistence(2);
    let anchor = |id: &str| MuxPaneAnchor {
        session_id: "session".into(),
        pane_id: Some(id.into()),
        ..Default::default()
    };
    let first = anchor("%first");
    let second = anchor("%second");
    terminal.sync_scoped_native_window(
        scope,
        std::slice::from_ref(&first),
        Some(&first),
        Some("first"),
        MuxBackendKind::Native,
        false,
    )?;
    terminal.prepare_scoped_native_panes(
        scope,
        std::slice::from_ref(&second),
        TerminalGeometry {
            cols: 60,
            rows: 20,
            cell_width: 10,
            cell_height: 20,
        },
    )?;
    terminal.prepare_scoped_native_panes(
        scope,
        std::slice::from_ref(&second),
        TerminalGeometry {
            cols: 60,
            rows: 20,
            cell_width: 10,
            cell_height: 20,
        },
    )?;
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    terminal.write_input(b"first")?;
    assert_eq!(terminal.focused_pane_id(), Some("%first"));
    anyhow::ensure!(terminal.scoped_terminal_runtime(scope, "%first").is_some());
    anyhow::ensure!(terminal.scoped_terminal_runtime(scope, "%second").is_some());
    anyhow::ensure!(
        terminal
            .scoped_terminal_runtime(other_scope, "%second")
            .is_none()
    );
    terminal.prepare_scoped_native_panes(
        other_scope,
        std::slice::from_ref(&second),
        TerminalGeometry {
            cols: 60,
            rows: 20,
            cell_width: 10,
            cell_height: 20,
        },
    )?;
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    anyhow::ensure!(
        terminal
            .scoped_terminal_runtime(other_scope, "%second")
            .is_some()
    );
    assert_eq!(terminal.focused_pane_id(), Some("%first"));
    terminal.sync_scoped_native_window(
        scope,
        std::slice::from_ref(&second),
        Some(&second),
        Some("second"),
        MuxBackendKind::Native,
        false,
    )?;
    assert_eq!(terminal.focused_pane_id(), Some("%second"));
    terminal.write_input(b"second")?;
    assert_eq!(
        *inputs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![
            ("%first".to_owned(), b"first".to_vec()),
            ("%second".to_owned(), b"second".to_vec()),
        ]
    );
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    anyhow::ensure!(terminal.scoped_terminal_runtime(scope, "%first").is_some());
    Ok(())
}

#[rstest]
#[case::create(false)]
#[case::update(true)]
fn a_missing_provider_leaves_saved_and_live_spaces_unchanged(#[case] update: bool) -> Result<()> {
    let directory = assert_fs::TempDir::new()?;
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    let registry = Arc::new(MuxBackendRegistry::from_app_providers(
        [Arc::new(StaleCacheProvider::native(Arc::default()))],
        [MuxBackendKind::Native],
    )?);
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    let mut workspace = WorkspaceRuntime::open(
        &config,
        "main",
        Arc::clone(&registry),
        AppearanceVariant::Light,
        Arc::clone(&repaint),
    )?;
    let before = workspace.space_summaries();
    let placement = SpaceMuxOverride {
        backend: Some(MultiplexerBackendConfig::Rmux),
        remote: SpaceRemoteOverride::Local,
    };
    let failed = if update {
        let mut summary = before.first().expect("initial Space").clone();
        summary.name = "Should not persist".to_owned();
        workspace
            .update_space(&summary, placement, &config, AppearanceVariant::Light)
            .is_err()
    } else {
        workspace
            .create_space(
                "Should not persist",
                "folder",
                [0, 0, 0],
                false,
                placement,
                &config,
                AppearanceVariant::Light,
            )
            .is_err()
    };
    anyhow::ensure!(failed, "a missing provider must reject the Space change");
    assert_eq!(workspace.space_summaries(), before);
    drop(workspace);
    let reopened =
        WorkspaceRuntime::open(&config, "main", registry, AppearanceVariant::Light, repaint)?;
    assert_eq!(reopened.space_summaries(), before);
    Ok(())
}

type InputLog = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

#[rstest]
#[case::keep_native(MultiplexerBackendConfig::Native)]
#[case::switch_to_rmux(MultiplexerBackendConfig::Rmux)]
#[case::switch_to_tmux(MultiplexerBackendConfig::Tmux)]
fn changing_a_spaces_backend_preserves_shared_native_panes(
    #[case] backend: MultiplexerBackendConfig,
) -> Result<()> {
    let directory = assert_fs::TempDir::new()?;
    let mut config = BoottyConfig {
        config_path: directory.path().join("config.toml"),
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    let starts = Arc::new(AtomicUsize::new(0));
    let mut providers = vec![Arc::new(StaleCacheProvider::native(Arc::clone(&starts)))];
    for kind in [MuxBackendKind::Rmux, MuxBackendKind::Tmux] {
        let mut provider = StaleCacheProvider::attach(Arc::clone(&starts), Arc::default());
        provider.kind = kind;
        providers.push(Arc::new(provider));
    }
    let registry = Arc::new(MuxBackendRegistry::from_app_providers(providers, [])?);
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    let variant = AppearanceVariant::Light;
    let mut workspace =
        WorkspaceRuntime::open(&config, "main", registry, variant, Arc::clone(&repaint))?;
    let original_scope = workspace.active_space_id();
    let edited_scope = workspace
        .create_space(
            "Edited",
            "folder",
            [0, 0, 0],
            false,
            SpaceMuxOverride::default(),
            &config,
            variant,
        )?
        .expect("valid Space");
    let panes = ["%first", "%second"].map(|pane| MuxPaneAnchor {
        session_id: "original".to_owned(),
        pane_id: Some(pane.to_owned()),
        ..MuxPaneAnchor::default()
    });
    workspace
        .active
        .binding
        .terminal_mut()
        .sync_scoped_native_window(
            original_scope,
            &panes,
            Some(&panes[0]),
            Some("original-window"),
            MuxBackendKind::Native,
            false,
        )?;
    workspace.activate_space(
        edited_scope,
        "main",
        &config,
        variant,
        &repaint,
        Instant::now(),
    )?;
    let edited_pane = MuxPaneAnchor {
        session_id: "edited".to_owned(),
        pane_id: Some("%edited".to_owned()),
        ..MuxPaneAnchor::default()
    };
    workspace
        .active
        .binding
        .terminal_mut()
        .sync_scoped_native_window(
            edited_scope,
            std::slice::from_ref(&edited_pane),
            Some(&edited_pane),
            Some("edited-window"),
            MuxBackendKind::Native,
            false,
        )?;
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    let summary = workspace
        .space_summaries()
        .into_iter()
        .find(|space| space.id == edited_scope)
        .unwrap();
    workspace.update_space(
        &summary,
        SpaceMuxOverride {
            backend: Some(backend),
            remote: SpaceRemoteOverride::Local,
        },
        &config,
        variant,
    )?;
    workspace.activate_space(
        original_scope,
        "main",
        &config,
        variant,
        &repaint,
        Instant::now(),
    )?;
    workspace
        .active
        .binding
        .terminal_mut()
        .sync_scoped_native_window(
            original_scope,
            &panes,
            Some(&panes[0]),
            Some("original-window"),
            MuxBackendKind::Native,
            false,
        )?;
    assert_eq!(
        starts.load(Ordering::SeqCst),
        3,
        "returning to the original Space must reuse both live panes"
    );

    // Changing the other Space back to native must recover the same shared owner as activation.
    workspace.activate_space(
        edited_scope,
        "main",
        &config,
        variant,
        &repaint,
        Instant::now(),
    )?;
    workspace.update_space(
        &summary,
        SpaceMuxOverride {
            backend: Some(MultiplexerBackendConfig::Native),
            remote: SpaceRemoteOverride::Local,
        },
        &config,
        variant,
    )?;
    workspace
        .active
        .binding
        .terminal_mut()
        .sync_scoped_native_window(
            edited_scope,
            std::slice::from_ref(&edited_pane),
            Some(&edited_pane),
            Some("edited-window"),
            MuxBackendKind::Native,
            false,
        )?;
    assert_eq!(
        starts.load(Ordering::SeqCst),
        3,
        "switching a Space back to native must reuse its live pane"
    );
    Ok(())
}

struct CachedPaneRuntime {
    pane: String,
    inputs: InputLog,
    fail_live_config: bool,
    fail_next_resize: AtomicBool,
    session: String,
    resized_sessions: Arc<Mutex<Vec<String>>>,
}

macro_rules! terminal_runtime_stubs {
    () => {
        fn drain_pty(&mut self) -> DrainStats {
            DrainStats::default()
        }
        fn pending_pty_len(&self) -> usize {
            0
        }
        fn child_exited(&mut self) -> Result<bool> {
            Ok(false)
        }
        fn tty_name(&self) -> Option<&str> {
            None
        }
        fn discard_pending_output(&mut self) -> Result<()> {
            Ok(())
        }
        fn force_resize(&mut self) -> Result<()> {
            Ok(())
        }
        fn format_selection(&mut self, _: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn current_working_directory(&mut self) -> Result<Option<String>> {
            Ok(None)
        }
        fn is_mouse_tracking(&mut self) -> Result<bool> {
            Ok(false)
        }
        fn scroll_viewport_delta(&mut self, _: isize) -> Result<()> {
            Ok(())
        }
        fn scroll_viewport_to(&mut self, _: usize) -> Result<()> {
            Ok(())
        }
        fn enter_copy_mode(&mut self) -> Result<()> {
            Ok(())
        }
        fn copy_mode_active(&mut self) -> Result<bool> {
            Ok(false)
        }
        fn handle_copy_mode_action(
            &mut self,
            _: TerminalCopyModeAction,
        ) -> Result<TerminalCopyModeOutcome> {
            Ok(TerminalCopyModeOutcome::default())
        }
        fn search_viewport(&mut self, _: &str, _: TerminalSearchDirection) -> Result<bool> {
            Ok(false)
        }
        fn begin_selection(&mut self, _: TerminalSelectionEvent) -> Result<()> {
            Ok(())
        }
        fn update_selection(&mut self, _: TerminalSelectionEvent) -> Result<()> {
            Ok(())
        }
        fn end_selection(&mut self, _: Option<TerminalSelectionEvent>) -> Result<()> {
            Ok(())
        }
        fn write_paste(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
        fn encode_key(&mut self, _: KeyInput) -> Result<()> {
            Ok(())
        }
        fn encode_focus(&mut self, _: bool) -> Result<()> {
            Ok(())
        }
        fn encode_mouse(&mut self, _: MouseInput) -> Result<()> {
            Ok(())
        }
        fn handle_mouse_wheel(&mut self, _: MouseInput, _: isize) -> Result<()> {
            Ok(())
        }
    };
}

impl TerminalFrameSource for CachedPaneRuntime {
    fn set_display_scale(&mut self, _: f32) -> Result<()> {
        Ok(())
    }
    fn set_render_cell_metrics(&mut self, _: CellMetrics) -> Result<()> {
        Ok(())
    }
    fn resize(&mut self, _: TerminalGeometry) -> Result<()> {
        self.resized_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(self.session.clone());
        if self.fail_next_resize.swap(false, Ordering::SeqCst) {
            anyhow::bail!("attach client closed")
        }
        Ok(())
    }
    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::default())
    }
}

impl TerminalRuntime for CachedPaneRuntime {
    terminal_runtime_stubs!();
    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.inputs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((self.pane.clone(), bytes.to_vec()));
        Ok(())
    }
    fn apply_live_config(&mut self, _config: TerminalLiveConfig) -> Result<()> {
        if self.fail_live_config {
            anyhow::bail!("cached pane is gone")
        }
        Ok(())
    }
}

#[derive(Arbitrary, Debug)]
struct ScopedPaneInput {
    first_scope: i64,
    second_scope: i64,
    pane_id: String,
}

proptest! {
    /// Property: scoped pane encoding is injective, and decoding is its left inverse.
    #[test]
    fn scoped_pane_ids_round_trip_without_cross_space_collisions(input in any::<ScopedPaneInput>()) {
    use bootty_mux::{
        controller::SpaceId,
        terminal::{decode_scoped_pane_id, encode_scoped_pane_id},
    };

    prop_assume!(input.first_scope != input.second_scope);
    let first = SpaceId::from_persistence(input.first_scope);
    let second = SpaceId::from_persistence(input.second_scope);

    let first_id = encode_scoped_pane_id(first, &input.pane_id);
    let second_id = encode_scoped_pane_id(second, &input.pane_id);

    prop_assert_ne!(&first_id, &second_id);
    prop_assert_eq!(
        decode_scoped_pane_id(&first_id),
        Some((first, input.pane_id))
    );
    }
}

struct EmptyBackend;

impl MuxBackend for EmptyBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(MuxSnapshot::default())
    }
    fn execute(&mut self, _: MuxCommand) -> Result<()> {
        Ok(())
    }
}

struct StaleCacheProvider {
    kind: MuxBackendKind,
    inputs: InputLog,
    starts: Arc<AtomicUsize>,
    behavior: PaneBehavior,
    resized_sessions: Arc<Mutex<Vec<String>>>,
}

struct StaleCachePolicy {
    inputs: InputLog,
    starts: Arc<AtomicUsize>,
    behavior: PaneBehavior,
    resized_sessions: Arc<Mutex<Vec<String>>>,
}

impl StaleCacheProvider {
    fn native(starts: Arc<AtomicUsize>) -> Self {
        Self {
            kind: MuxBackendKind::Native,
            inputs: Arc::default(),
            starts,
            behavior: PaneBehavior {
                topology: PaneTopology::ProcessLocal,
                cache_terminals: true,
                resize_cached_terminals: false,
            },
            resized_sessions: Arc::default(),
        }
    }

    fn attach(starts: Arc<AtomicUsize>, resized_sessions: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            kind: MuxBackendKind::Native,
            inputs: Arc::default(),
            starts,
            behavior: PaneBehavior {
                topology: PaneTopology::Attach,
                cache_terminals: true,
                resize_cached_terminals: true,
            },
            resized_sessions,
        }
    }
}

impl MuxBackendProvider for StaleCacheProvider {
    fn kind(&self) -> MuxBackendKind {
        self.kind
    }
    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::CallerThread
    }
    fn build_backend(&self, _: &MuxBindingConfig, _: Option<&Path>) -> Box<dyn MuxBackend> {
        Box::new(EmptyBackend)
    }
}

impl MuxAppBackendProvider for StaleCacheProvider {
    fn build_pane_policy(&self, _config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(StaleCachePolicy {
            inputs: Arc::clone(&self.inputs),
            starts: Arc::clone(&self.starts),
            behavior: self.behavior,
            resized_sessions: Arc::clone(&self.resized_sessions),
        })
    }

    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: self.behavior,
            progress: TerminalProgressPolicy::TerminalOsc,
            persisted_sessions: PersistedSessionPolicy::Never,
            generated_session_names: GeneratedSessionNamePolicy::Reconcile,
            terminal_residency: if self.behavior.topology == PaneTopology::ProcessLocal {
                TerminalResidency::WorkspaceShared
            } else {
                TerminalResidency::BindingScoped
            },
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        BindingCapabilityDescriptor::new(scope, [])
    }
}

impl BackendPanePolicy for StaleCachePolicy {
    fn remote_target(&self) -> Option<bootty_mux::RemoteTarget> {
        None
    }
    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        let start = self.starts.fetch_add(1, Ordering::SeqCst);
        if start == 0 && request.target.pane_id() == Some("%anchor") {
            return Ok(None);
        }
        Ok(Some(Box::new(CachedPaneRuntime {
            pane: request.target.pane_id().unwrap_or_default().to_owned(),
            inputs: Arc::clone(&self.inputs),
            fail_live_config: request.target.pane_id() == Some("%2"),
            fail_next_resize: AtomicBool::new(
                self.behavior.topology == PaneTopology::Attach
                    && request.target.session_id() == "stale",
            ),
            session: request.target.session_id().to_owned(),
            resized_sessions: Arc::clone(&self.resized_sessions),
        })))
    }

    fn sync_target(&mut self, _: Option<&bootty_mux::terminal::ScopedMuxPaneTarget>, _: bool) {}
    fn set_layout_window(&mut self, _: Option<&str>) {}
    fn resize_layout_window(&mut self, _: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        Ok(false)
    }
    fn deactivate(&mut self) {}
}

fn attach_resize_terminal(
    starts: Arc<AtomicUsize>,
    resized_sessions: Arc<Mutex<Vec<String>>>,
) -> Result<bootty_mux::terminal::ActiveTerminal> {
    let registry = Arc::new(MuxBackendRegistry::from_app_providers(
        [Arc::new(StaleCacheProvider::attach(
            starts,
            resized_sessions,
        ))],
        [MuxBackendKind::Native],
    )?);
    bootty_mux::terminal::ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        registry,
        &MuxBindingConfig {
            backend: MuxBackendKind::Native,
            ..MuxBindingConfig::default()
        },
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(|| {}),
    )
}

fn session_anchor(session: &str) -> MuxPaneAnchor {
    MuxPaneAnchor {
        session_id: session.to_owned(),
        ..MuxPaneAnchor::default()
    }
}

#[rstest::rstest]
fn idle_terminal_reuses_its_published_frame() -> Result<()> {
    let starts = Arc::new(AtomicUsize::new(0));
    let registry = Arc::new(MuxBackendRegistry::from_app_providers(
        [Arc::new(StaleCacheProvider::native(starts))],
        [MuxBackendKind::Native],
    )?);
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Native,
        ..MuxBindingConfig::default()
    };
    let mut terminal = bootty_mux::terminal::ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        registry,
        &config,
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(|| {}),
    )?;

    let first = terminal.extract_frame()?;
    let second = terminal.extract_frame()?;

    anyhow::ensure!(Arc::ptr_eq(&first, &second));
    Ok(())
}

#[rstest::rstest]
fn failed_active_attach_resize_is_retried_at_the_same_geometry() -> Result<()> {
    let starts = Arc::new(AtomicUsize::new(0));
    let resized_sessions = Arc::new(Mutex::new(Vec::new()));
    let mut terminal = attach_resize_terminal(starts, Arc::clone(&resized_sessions))?;
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Native,
        ..MuxBindingConfig::default()
    };
    terminal.sync_mux_anchor(&config, Some(&session_anchor("stale")))?;
    let geometry = TerminalGeometry {
        cols: 100,
        rows: 30,
        cell_width: 10,
        cell_height: 20,
    };

    anyhow::ensure!(terminal.resize(geometry).is_err());
    terminal.resize(geometry)?;

    assert_eq!(
        resized_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_slice(),
        ["stale", "stale"]
    );
    Ok(())
}

#[rstest::rstest]
fn dead_cached_attach_does_not_block_visible_resize_and_is_recreated() -> Result<()> {
    let starts = Arc::new(AtomicUsize::new(0));
    let resized_sessions = Arc::new(Mutex::new(Vec::new()));
    let mut terminal = attach_resize_terminal(Arc::clone(&starts), Arc::clone(&resized_sessions))?;
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Native,
        ..MuxBindingConfig::default()
    };
    let stale = session_anchor("stale");
    let visible = session_anchor("visible");
    terminal.sync_mux_anchor(&config, Some(&stale))?;
    terminal.sync_mux_anchor(&config, Some(&visible))?;

    terminal.resize(TerminalGeometry {
        cols: 100,
        rows: 30,
        cell_width: 10,
        cell_height: 20,
    })?;

    assert_eq!(
        resized_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_slice(),
        ["visible", "stale"]
    );
    terminal.sync_mux_anchor(&config, Some(&stale))?;
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    Ok(())
}

#[rstest]
#[case("%anchor", 2)]
#[case("%ready", 1)]
fn first_native_window_sync_materializes_only_idle_anchors(
    #[case] pane_id: &str,
    #[case] expected_starts: usize,
) -> Result<()> {
    let starts = Arc::new(AtomicUsize::new(0));
    let registry = Arc::new(MuxBackendRegistry::from_app_providers(
        [Arc::new(StaleCacheProvider::native(Arc::clone(&starts)))],
        [MuxBackendKind::Native],
    )?);
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Native,
        ..MuxBindingConfig::default()
    };
    let focused = MuxPaneAnchor {
        session_id: "session".into(),
        pane_id: Some(pane_id.into()),
        ..MuxPaneAnchor::default()
    };
    let mut terminal = bootty_mux::terminal::ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        registry,
        &config,
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(|| {}),
    )?;

    terminal.sync_mux_anchor(&config, Some(&focused))?;
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    terminal.sync_native_window(
        std::slice::from_ref(&focused),
        Some(&focused),
        Some("window"),
        MuxBackendKind::Native,
        false,
    )?;
    assert_eq!(starts.load(Ordering::SeqCst), expected_starts);

    terminal.sync_native_window(
        std::slice::from_ref(&focused),
        Some(&focused),
        Some("window"),
        MuxBackendKind::Native,
        false,
    )?;
    assert_eq!(starts.load(Ordering::SeqCst), expected_starts);
    Ok(())
}

#[test]
fn failed_cached_runtime_is_retired_without_blocking_live_config_publication() -> Result<()> {
    let starts = Arc::new(AtomicUsize::new(0));
    let registry = Arc::new(MuxBackendRegistry::from_app_providers(
        [Arc::new(StaleCacheProvider::native(Arc::clone(&starts)))],
        [MuxBackendKind::Native],
    )?);
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Native,
        ..MuxBindingConfig::default()
    };
    let pane = |id: &str| MuxPaneAnchor {
        session_id: "session".into(),
        pane_id: Some(id.into()),
        ..MuxPaneAnchor::default()
    };
    let (first, stale) = (pane("%1"), pane("%2"));
    let mut terminal = bootty_mux::terminal::ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        registry,
        &config,
        bootty_terminal::TerminalSessionConfig::default(),
        Arc::new(|| {}),
    )?;

    terminal.sync_native_window(
        &[first.clone(), stale.clone()],
        Some(&first),
        Some("window"),
        MuxBackendKind::Native,
        false,
    )?;
    terminal.apply_live_config(TerminalLiveConfig::default())?;
    assert_eq!(starts.load(Ordering::SeqCst), 2);

    terminal.sync_native_window(
        &[first.clone(), stale],
        Some(&first),
        Some("window"),
        MuxBackendKind::Native,
        false,
    )?;
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    Ok(())
}
