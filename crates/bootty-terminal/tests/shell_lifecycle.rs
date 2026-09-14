use bootty_terminal::{
    geometry::TerminalGeometry,
    shell_lifecycle::{ShellEvent, ShellLifecycle},
    terminal_engine::TerminalEngine,
    terminal_side_effect::TerminalSideEffect,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use std::time::{Duration, Instant};

proptest! {
    #[test]
    fn completion_uses_first_start_and_consumes_it_once(seconds in 1u64..86400, repeats in 0..8, code in 0i32..256) {
        let start=Instant::now(); let mut lifecycle=ShellLifecycle::default();
        prop_assert!(lifecycle.apply(ShellEvent::CommandStart,start).is_none());
        for _ in 0..repeats { lifecycle.apply(ShellEvent::CommandStart,start.checked_add(Duration::from_millis(1)).unwrap()); }
        let finish=ShellEvent::CommandFinish { exit_code:Some(code) };
        let completion=lifecycle.apply(finish,start.checked_add(Duration::from_secs(seconds)).unwrap()).unwrap();
        prop_assert_eq!(completion.elapsed,Duration::from_secs(seconds));
        prop_assert_eq!(completion.exit_code,Some(code));
        prop_assert!(lifecycle.apply(finish,start.checked_add(Duration::from_secs(seconds.checked_add(1).unwrap())).unwrap()).is_none());
    }
}
#[rstest]
fn prompt_without_finish_cannot_complete_an_old_command() {
    let now = Instant::now();
    let mut lifecycle = ShellLifecycle::default();
    lifecycle.apply(ShellEvent::CommandStart, now);
    lifecycle.apply(ShellEvent::PromptStart, now);
    assert!(
        lifecycle
            .apply(ShellEvent::CommandFinish { exit_code: Some(0) }, now)
            .is_none()
    );
    assert_eq!(ShellEvent::parse_osc133("D;invalid"), None);
    assert_eq!(ShellEvent::parse_osc133("V;1"), None);
}
#[rstest]
fn replay_restores_title_without_repeating_bells_or_completions() {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    })
    .unwrap();
    let bytes = b"\x07\x1b]133;C\x07\x1b]133;D;7\x07\x1b]2;title\x07";
    engine.write_vt_without_pty_responses(bytes);
    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::WindowTitle("title".into())]
    );
    engine.write_vt(bytes);
    assert_eq!(
        engine.drain_side_effects(),
        vec![
            TerminalSideEffect::ShellLifecycle(ShellEvent::CommandStart),
            TerminalSideEffect::ShellLifecycle(ShellEvent::CommandFinish { exit_code: Some(7) }),
            TerminalSideEffect::Bell,
            TerminalSideEffect::WindowTitle("title".into())
        ]
    );
}
