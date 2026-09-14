#![cfg(test)]

use bootty_terminal::terminal::TerminalKey;
use bootty_ui::keymap::{
    AdjustSelection, BindingAction, BindingFlags, BindingKey, BindingModSide, BindingMods,
    BindingParseError, BindingTrigger, CopyToClipboard, NavigateSearch, WriteScreen,
    WriteScreenAction, WriteScreenFormat, parse_action, parse_binding,
};
use pretty_assertions::assert_eq;

#[test]
fn binding_parser_preserves_triggers_flags_and_modifier_sides() {
    for (input, mods, key) in [
        (
            "shift+ctrl+a=ignore",
            BindingMods {
                shift: true,
                ctrl: true,
                ..BindingMods::default()
            },
            BindingKey::Unicode('a'),
        ),
        (
            "ctrl++=ignore",
            BindingMods {
                ctrl: true,
                ..BindingMods::default()
            },
            BindingKey::Unicode('+'),
        ),
        (
            "alt+scroll_up=ignore",
            BindingMods {
                alt: true,
                ..BindingMods::default()
            },
            BindingKey::ScrollUp,
        ),
        (
            "alt+scroll_down=ignore",
            BindingMods {
                alt: true,
                ..BindingMods::default()
            },
            BindingKey::ScrollDown,
        ),
    ] {
        assert_eq!(
            parse_binding(input).expect("binding parses").trigger,
            BindingTrigger { mods, key },
            "{input}"
        );
    }

    let binding = parse_binding("unconsumed:performable:left_ctrl+right_alt+a=ignore")
        .expect("sided binding parses");
    assert_eq!(
        binding.trigger.mods,
        BindingMods {
            ctrl: true,
            alt: true,
            ctrl_side: Some(BindingModSide::Left),
            alt_side: Some(BindingModSide::Right),
            ..BindingMods::default()
        }
    );
    assert_eq!(binding.trigger.format_entry(), "left_ctrl+right_alt+a");
    assert_eq!(
        binding.flags,
        BindingFlags {
            consumed: false,
            performable: true,
            ..BindingFlags::default()
        }
    );

    for input in [
        "foo=ignore",
        "shift+shift+a=ignore",
        "a+b=ignore",
        "alt+right_alt+a=ignore",
    ] {
        assert_eq!(
            parse_binding(input),
            Err(BindingParseError::InvalidFormat),
            "{input}"
        );
    }
}

#[test]
fn binding_parser_preserves_physical_keys_aliases_and_catch_all() {
    for (input, key, canonical) in [
        ("KeyA", TerminalKey::A, "KeyA"),
        ("key_a", TerminalKey::A, "KeyA"),
        ("Enter", TerminalKey::Enter, "Enter"),
        ("enter", TerminalKey::Enter, "Enter"),
    ] {
        let parsed = parse_binding(&format!("{input}=ignore")).expect("physical key parses");
        assert_eq!(parsed.trigger.key, BindingKey::Physical(key), "{input}");
        assert_eq!(parsed.trigger.format_entry(), canonical, "{input}");
    }

    assert_eq!(
        parse_binding("physical:zero=ignore")
            .expect("legacy physical key parses")
            .trigger
            .key,
        BindingKey::Physical(TerminalKey::Digit0)
    );
    assert_eq!(
        parse_binding("ctrl+catch_all=ignore")
            .expect("catch-all parses")
            .trigger,
        BindingTrigger {
            mods: BindingMods {
                ctrl: true,
                ..BindingMods::default()
            },
            key: BindingKey::CatchAll,
        }
    );
    assert_eq!(
        parse_binding("Keya=ignore"),
        Err(BindingParseError::InvalidFormat)
    );
}

#[test]
fn binding_keys_resolve_shifted_symbols_from_terminal_key_text() {
    for (key, shifted) in [
        (BindingKey::Unicode('['), "{"),
        (BindingKey::Unicode(']'), "}"),
        (BindingKey::Unicode(','), "<"),
        (BindingKey::Physical(TerminalKey::Digit1), "!"),
        (BindingKey::Physical(TerminalKey::Slash), "?"),
    ] {
        assert_eq!(key.shifted_symbol_utf8(), Some(shifted), "{key:?}");
    }

    assert_eq!(BindingKey::Unicode('a').shifted_symbol_utf8(), None);
    assert_eq!(
        BindingKey::Physical(TerminalKey::A).shifted_symbol_utf8(),
        None
    );
    assert_eq!(
        BindingKey::Physical(TerminalKey::Space).shifted_symbol_utf8(),
        None
    );
}

#[test]
fn binding_parser_preserves_action_grammar_and_errors() {
    assert_valid_action_bindings();

    assert_invalid_bindings([
        "a=nopenopenope",
        "a=ignore:A",
        "a=reset:A",
        "a=csi",
        "a=esc",
        "a=text",
        "a=copy_to_clipboard:invalid",
        "a=navigate_search:sideways",
        "a=increase_font_size:nope",
        "a=set_font_size:nan",
        "a=scroll_page_fractional:inf",
        "a=scroll_page_lines:100000",
        "a=adjust_selection:middle",
        "a=write_screen_file:copy,html,extra",
    ]);
}

fn assert_valid_action_bindings() {
    for (input, action) in [
        ("a=ignore", BindingAction::Ignore),
        ("a=unbind", BindingAction::Unbind),
        ("a=reset", BindingAction::Reset),
        ("a=reload_config", BindingAction::ReloadConfig),
        ("a=new_window", BindingAction::NewWindow),
        ("a=close_window", BindingAction::CloseWindow),
        ("a=close_surface", BindingAction::CloseSurface),
        ("a=quit", BindingAction::Quit),
        ("a=toggle_fullscreen", BindingAction::ToggleFullscreen),
        ("a=open_settings", BindingAction::OpenSettings),
        ("a=csi:A", BindingAction::Csi("A".to_owned())),
        ("a=esc:7", BindingAction::Esc("7".to_owned())),
        ("a=text:=hello", BindingAction::Text("=hello".to_owned())),
        ("a=create_space", BindingAction::CreateSpace),
        ("a=close_space", BindingAction::CloseSpace),
        ("a=edit_space", BindingAction::EditSpace),
        ("a=next_space", BindingAction::NextSpace),
        ("a=previous_space", BindingAction::PreviousSpace),
        ("a=select_space:3", BindingAction::SelectSpace(3)),
        (
            "a=search:needle",
            BindingAction::Search("needle".to_owned()),
        ),
        ("a=search_selection", BindingAction::SearchSelection),
        (
            "a=navigate_search:previous",
            BindingAction::NavigateSearch(NavigateSearch::Previous),
        ),
        (
            "a=copy_to_clipboard:html",
            BindingAction::CopyToClipboard(CopyToClipboard::Html),
        ),
        (
            "a=increase_font_size:1.5",
            BindingAction::IncreaseFontSize(1.5),
        ),
        ("a=set_font_size:13.5", BindingAction::SetFontSize(13.5)),
        ("a=scroll_to_row:12", BindingAction::ScrollToRow(12)),
        (
            "a=scroll_page_fractional:-0.5",
            BindingAction::ScrollPageFractional(-0.5),
        ),
        (
            "a=scroll_page_lines:-10",
            BindingAction::ScrollPageLines(-10),
        ),
        (
            "a=adjust_selection:beginning_of_line",
            BindingAction::AdjustSelection(AdjustSelection::BeginningOfLine),
        ),
        ("a=jump_to_prompt:-1", BindingAction::JumpToPrompt(-1)),
        (
            "a=write_scrollback_file:paste,vt",
            BindingAction::WriteScrollbackFile(WriteScreen {
                action: WriteScreenAction::Paste,
                emit: WriteScreenFormat::Vt,
            }),
        ),
        (
            "a=write_screen_file:copy,html",
            BindingAction::WriteScreenFile(WriteScreen {
                action: WriteScreenAction::Copy,
                emit: WriteScreenFormat::Html,
            }),
        ),
        (
            "a=activate_key_table:copy-mode",
            BindingAction::ActivateKeyTable("copy-mode".to_owned()),
        ),
        (
            "a=toggle_mouse_reporting",
            BindingAction::ToggleMouseReporting,
        ),
    ] {
        assert_eq!(
            parse_binding(input)
                .expect("parameterized action parses")
                .action,
            action,
            "{input}"
        );
    }
}

fn assert_invalid_bindings(inputs: [&str; 14]) {
    for input in inputs {
        assert!(parse_binding(input).is_err(), "{input}");
    }
}

#[test]
fn binding_action_grammar_preserves_defaults_and_validation() {
    for (input, action, canonical) in [
        (
            "copy_to_clipboard",
            BindingAction::CopyToClipboard(CopyToClipboard::Mixed),
            "copy_to_clipboard:mixed",
        ),
        (
            "write_screen_file:open",
            BindingAction::WriteScreenFile(WriteScreen {
                action: WriteScreenAction::Open,
                emit: WriteScreenFormat::Plain,
            }),
            "write_screen_file:open,plain",
        ),
        ("select_tab:1", BindingAction::SelectTab(1), "select_tab:1"),
        (
            "select_space:2",
            BindingAction::SelectSpace(2),
            "select_space:2",
        ),
        (
            "select_session:3",
            BindingAction::SelectSession(3),
            "select_session:3",
        ),
        (
            "set_surface_title:\u{1f47b}",
            BindingAction::SetSurfaceTitle("\u{1f47b}".to_owned()),
            "set_surface_title:\\xf0\\x9f\\x91\\xbb",
        ),
    ] {
        assert_eq!(parse_action(input), Ok(action.clone()), "{input}");
        assert_eq!(action.format_entry(), canonical, "{input}");
    }

    for input in [
        "ignore:value",
        "set_surface_title",
        "set_font_size:nan",
        "select_tab:0",
        "select_space:0",
        "select_session:0",
        "select_pane:sideways",
        "write_screen_file:copy,html,extra",
    ] {
        assert_eq!(
            parse_action(input),
            Err(BindingParseError::InvalidFormat),
            "{input}"
        );
    }
}
