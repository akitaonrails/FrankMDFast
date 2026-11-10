//! Level-b (headless GPUI) tests of the editor pane: document in/out, edit events,
//! formatting shortcuts, the find bar, the text format menu, the history boundary
//! and typewriter mode (docs/testing/ui-test-plan.md §9 codemirror_controller,
//! §11 find_replace, §13 text_format / typewriter).
#![allow(missing_docs, clippy::unwrap_used)]

extern crate gpui_kit as gpui;

mod editor_support;

use editor_support::{Fixture, open};
use frankmd_app::editor::{EditorEvent, EditorSettings};
use gpui_kit::TestAppContext;

fn edited_count(events: &[EditorEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, EditorEvent::Edited { .. }))
        .count()
}

fn click(f: &Fixture, cx: &mut TestAppContext, id: &'static str) {
    use gpui_kit::test::TestWindowExt as _;
    f.with(cx, |_, window, cx| window.click(id, cx));
    cx.run_until_parked();
}

fn undo(f: &Fixture, cx: &mut TestAppContext) {
    f.press(cx, "ctrl-z");
}

// adapts: codemirror_controller.test.js:54 "initializes with content from value"
// adapts: codemirror_extensions.test.js:51 "returns an array of extensions" (opens with defaults, focused)
#[gpui_kit::test]
fn opening_a_note_shows_its_text_and_focuses_the_editor(cx: &mut TestAppContext) {
    let f = open(cx, "Hello World", EditorSettings::default());
    assert_eq!(f.text(cx), "Hello World");
    assert!(f.is_shown(cx, "editor"));
    assert!(f.has_focus(cx, "editor"));
    // set_document is not a user edit.
    assert_eq!(edited_count(&f.take_events()), 0);
}

// adapts: codemirror_controller.test.js:76 "returns the full document text"
// adapts: codemirror_controller.test.js:903 "dispatches change event on document change"
#[gpui_kit::test]
fn typing_reports_edits_and_the_full_text(cx: &mut TestAppContext) {
    let f = open(cx, "", EditorSettings::default());
    f.type_text(cx, "abc");
    assert_eq!(f.text(cx), "abc");
    assert!(edited_count(&f.take_events()) >= 1);
}

#[gpui_kit::test]
fn caret_moves_are_reported_with_line_and_column(cx: &mut TestAppContext) {
    let f = open(cx, "one\ntwo", EditorSettings::default());
    f.take_events();
    f.select(cx, 5..5);
    let events = f.take_events();
    assert!(
        events.contains(&EditorEvent::SelectionChanged {
            selection: 5..5,
            line: 2,
            column: 2
        }),
        "{events:?}"
    );
}

// Ctrl+B wraps, a second Ctrl+B unwraps; each is one undo step (codemirror_extensions wrapSelection).
#[gpui_kit::test]
fn bold_shortcut_toggles_and_undoes_in_one_step(cx: &mut TestAppContext) {
    let f = open(cx, "a word b", EditorSettings::default());
    f.select(cx, 2..6);
    f.press(cx, "ctrl-b");
    assert_eq!(f.text(cx), "a **word** b");
    assert_eq!(f.selection(cx), 4..8);
    f.press(cx, "ctrl-b");
    assert_eq!(f.text(cx), "a word b");
    f.press(cx, "ctrl-b");
    undo(&f, cx);
    assert_eq!(f.text(cx), "a word b");
}

#[gpui_kit::test]
fn italic_strike_code_and_link_shortcuts(cx: &mut TestAppContext) {
    let f = open(cx, "x", EditorSettings::default());
    f.select(cx, 0..1);
    f.press(cx, "ctrl-i");
    assert_eq!(f.text(cx), "*x*");
    f.select(cx, 0..3);
    f.press(cx, "ctrl-shift-s");
    assert_eq!(f.text(cx), "~~*x*~~");
    f.select(cx, 7..7);
    f.press(cx, "ctrl-`");
    assert_eq!(f.text(cx), "~~*x*~~``");
    f.select(cx, 0..9);
    f.press(cx, "ctrl-k");
    assert_eq!(f.text(cx), "[~~*x*~~``](url)");
    assert_eq!(f.selection(cx), 12..15, "url is selected");
}

// Ctrl+Enter toggles the task marker / turns the line into a task (toggleTask).
#[gpui_kit::test]
fn ctrl_enter_toggles_tasks(cx: &mut TestAppContext) {
    let f = open(cx, "buy milk\n- [ ] call", EditorSettings::default());
    f.select(cx, 3..3);
    f.press(cx, "ctrl-enter");
    assert_eq!(f.text(cx), "- [ ] buy milk\n- [ ] call");
    assert_eq!(
        f.selection(cx),
        9..9,
        "caret maps through the inserted marker"
    );
    f.select(cx, 0..20);
    f.press(cx, "ctrl-enter");
    assert_eq!(f.text(cx), "- [x] buy milk\n- [x] call");
}

// adapts: codemirror_controller.test.js:355 "routes exhausted Ctrl+Shift+Z to the creation boundary"
#[gpui_kit::test]
fn redo_with_no_history_reports_the_boundary_once(cx: &mut TestAppContext) {
    let f = open(cx, "abc", EditorSettings::default());
    f.take_events();
    f.press(cx, "ctrl-shift-z");
    let boundaries = f
        .take_events()
        .iter()
        .filter(|e| **e == EditorEvent::RedoBoundary)
        .count();
    assert_eq!(boundaries, 1);
    f.press(cx, "ctrl-z");
    assert!(f.take_events().contains(&EditorEvent::UndoBoundary));
    // With history, undo is a normal undo.
    f.type_text(cx, "x");
    f.take_events();
    f.press(cx, "ctrl-z");
    assert_eq!(f.text(cx), "abc");
    assert!(!f.take_events().contains(&EditorEvent::UndoBoundary));
}

// adapts: find_replace_controller.test.js:80 "opens dialog in replace tab"
#[gpui_kit::test]
fn ctrl_h_opens_the_find_bar_with_replace_prefilled_from_selection(cx: &mut TestAppContext) {
    let f = open(cx, "one two one", EditorSettings::default());
    f.select(cx, 0..3);
    f.press(cx, "ctrl-h");
    assert!(f.is_shown(cx, "find-bar"));
    assert!(f.is_shown(cx, "find-input"));
    assert!(f.is_shown(cx, "replace-input"));
    assert!(f.has_focus(cx, "find-input"));
    let (query, total) = f.pane.read_with(cx, |p, _| {
        (p.find_state().query.clone(), p.find_state().ranges().len())
    });
    assert_eq!((query.as_str(), total), ("one", 2));
}

#[gpui_kit::test]
fn ctrl_f_opens_find_only_and_escape_returns_to_the_editor_with_the_match(cx: &mut TestAppContext) {
    let f = open(cx, "one two one", EditorSettings::default());
    f.press(cx, "ctrl-f");
    assert!(f.is_shown(cx, "find-bar"));
    assert!(!f.is_shown(cx, "replace-input"));
    f.type_text(cx, "one");
    f.press(cx, "enter");
    assert_eq!(f.selection(cx), 0..3);
    f.press(cx, "enter");
    assert_eq!(f.selection(cx), 8..11);
    f.press(cx, "shift-enter");
    assert_eq!(f.selection(cx), 0..3);
    f.press(cx, "escape");
    assert!(!f.is_shown(cx, "find-bar"));
    assert!(f.has_focus(cx, "editor"));
    assert_eq!(f.selection(cx), 0..3);
    assert!(f.take_events().contains(&EditorEvent::FindClosed));
}

#[gpui_kit::test]
fn find_bar_counts_matches_live_and_reports_regex_errors(cx: &mut TestAppContext) {
    let f = open(cx, "a a a", EditorSettings::default());
    f.press(cx, "ctrl-f");
    f.type_text(cx, "a");
    assert_eq!(
        f.pane.read_with(cx, |p, _| p.find_state().ranges().len()),
        3
    );
    // Live on document edits (debounced).
    f.with(cx, |pane, window, cx| {
        pane.update(cx, |p, cx| p.insert("a ", window, cx))
    });
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(
        f.pane.read_with(cx, |p, _| p.find_state().ranges().len()),
        4
    );
    // Alt+R: regex; "(" is invalid.
    f.press(cx, "alt-r");
    f.press(cx, "backspace");
    f.type_text(cx, "(");
    assert!(
        f.pane
            .read_with(cx, |p, _| p.find_state().error().is_some())
    );
}

// adapts: find_replace_controller.test.js:144 "shows visual feedback after replace all" (wiring)
#[gpui_kit::test]
fn replace_all_is_one_undo_step(cx: &mut TestAppContext) {
    let f = open(cx, "a a a", EditorSettings::default());
    f.press(cx, "ctrl-h");
    f.type_text(cx, "a");
    click(&f, cx, "replace-input");
    f.type_text(cx, "b");
    f.press(cx, "ctrl-alt-enter");
    assert_eq!(f.text(cx), "b b b");
    f.with(cx, |pane, window, cx| {
        pane.update(cx, |p, cx| p.focus(window, cx))
    });
    undo(&f, cx);
    assert_eq!(f.text(cx), "a a a");
}

#[gpui_kit::test]
fn alt_enter_replaces_the_current_match_and_advances(cx: &mut TestAppContext) {
    let f = open(cx, "x a a", EditorSettings::default());
    f.press(cx, "ctrl-h");
    f.type_text(cx, "a");
    click(&f, cx, "replace-input");
    f.type_text(cx, "bb");
    f.press(cx, "alt-enter");
    assert_eq!(f.text(cx), "x bb a");
    assert_eq!(f.selection(cx), 5..6, "next match selected");
}

// adapts: text_format_controller.test.js:186 "renders a button for each format"
#[gpui_kit::test]
fn ctrl_m_opens_the_format_menu_with_seven_rows(cx: &mut TestAppContext) {
    let f = open(cx, "some text", EditorSettings::default());
    // No selection: nothing opens (openFromKeyboard returns early).
    f.press(cx, "ctrl-m");
    assert!(!f.is_shown(cx, "text-format-menu"));
    f.select(cx, 0..4);
    f.press(cx, "ctrl-m");
    assert!(f.is_shown(cx, "text-format-menu"));
    for id in [
        "text-format-row-bold",
        "text-format-row-italic",
        "text-format-row-strikethrough",
        "text-format-row-highlight",
        "text-format-row-subscript",
        "text-format-row-superscript",
        "text-format-row-link",
    ] {
        assert!(f.is_shown(cx, id), "{id}");
    }
    f.press(cx, "escape");
    assert!(!f.is_shown(cx, "text-format-menu"));
    assert!(f.has_focus(cx, "editor"));
    assert_eq!(f.text(cx), "some text");
}

// adapts: text_format_controller.test.js:398 "applies format on click"
#[gpui_kit::test]
fn clicking_a_format_row_applies_it_and_closes_the_menu(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    let f = open(cx, "some text", EditorSettings::default());
    f.select(cx, 0..4);
    f.press(cx, "ctrl-m");
    f.with(cx, |_, window, cx| {
        window.click("text-format-row-strikethrough", cx)
    });
    cx.run_until_parked();
    assert_eq!(f.text(cx), "~~some~~ text");
    assert!(!f.is_shown(cx, "text-format-menu"));
}

// Mnemonics and arrows (text_format_controller onMenuKeydown).
#[gpui_kit::test]
fn format_menu_mnemonics_and_enter(cx: &mut TestAppContext) {
    let f = open(cx, "some text", EditorSettings::default());
    f.select(cx, 5..9);
    f.press(cx, "ctrl-m");
    f.press(cx, "h");
    assert_eq!(f.text(cx), "some ==text==");
    f.select(cx, 0..4);
    f.press(cx, "ctrl-m");
    f.press(cx, "up enter");
    assert_eq!(
        f.text(cx),
        "[some](url) ==text==",
        "up wraps to the last row (link)"
    );
    assert_eq!(f.selection(cx), 7..10);
}

// Typewriter (FE A.10): the caret row stays centred while moving down.
#[gpui_kit::test]
fn typewriter_keeps_the_caret_near_the_middle(cx: &mut TestAppContext) {
    let text: String = (0..400).map(|i| format!("line {i}\n")).collect();
    let f = open(
        cx,
        &text,
        EditorSettings {
            typewriter: true,
            ..EditorSettings::default()
        },
    );
    let target = text.find("line 200").unwrap();
    f.select(cx, target..target);
    let (caret_y, top, height) = f.with(cx, |pane, _, cx| {
        let state = pane.read(cx).editor_state().read(cx);
        let caret = state
            .range_to_bounds(&(target..target))
            .expect("caret laid out");
        let bounds = state.input_bounds();
        (caret.top(), bounds.top(), bounds.size.height)
    });
    let ratio = (caret_y - top) / height;
    assert!(
        (0.35..=0.65).contains(&ratio),
        "caret at {ratio} of the viewport"
    );
}

fn scrolled_count(events: &[EditorEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, EditorEvent::Scrolled { .. }))
        .count()
}

// Sync scroll from the preview must not echo back (the echo snapped the preview back).
#[gpui_kit::test]
fn sync_scroll_does_not_echo_but_plain_scroll_does(cx: &mut TestAppContext) {
    let text: String = (0..400).map(|i| format!("line {i}\n")).collect();
    let f = open(cx, &text, EditorSettings::default());
    f.take_events();
    f.with(cx, |pane, _, cx| {
        pane.update(cx, |p, cx| p.sync_to_line(100, 0.0, cx))
    });
    cx.run_until_parked();
    f.with(cx, |_, _, _| ());
    assert_eq!(scrolled_count(&f.take_events()), 0);
    let top = f.with(cx, |pane, _, cx| pane.read(cx).top_visible_line(cx));
    assert_eq!(top, 100);

    // Navigation (open note at a line) still reports, so the preview follows.
    f.with(cx, |pane, _, cx| {
        pane.update(cx, |p, cx| p.scroll_to_line(200, cx))
    });
    cx.run_until_parked();
    f.with(cx, |pane, _, cx| pane.update(cx, |_, cx| cx.notify()));
    cx.run_until_parked();
    assert!(scrolled_count(&f.take_events()) >= 1);
}

// Sync scroll keeps its place inside a soft-wrapped line (long paragraphs used to
// hold still, then jump a whole line).
#[gpui_kit::test]
fn sync_scroll_tracks_position_inside_a_wrapped_line(cx: &mut TestAppContext) {
    let long = "word ".repeat(4000);
    let text = format!("intro\n{long}\nend\n");
    let f = open(cx, &text, EditorSettings::default());
    f.with(cx, |pane, _, cx| {
        pane.update(cx, |p, cx| p.sync_to_line(1, 0.5, cx))
    });
    cx.run_until_parked();
    f.take_events();
    let top = f.with(cx, |pane, _, cx| pane.read(cx).top_visible_line(cx));
    assert_eq!(top, 1, "half-way into the wrapped line keeps it at the top");

    // A small wheel tick inside the same line reports progress through it.
    f.with(cx, |_, window, cx| {
        use gpui_kit::test::TestWindowExt as _;
        window.scroll("editor", gpui::ScrollDelta::Lines(gpui::point(0., -3.)), cx)
    });
    cx.run_until_parked();
    let reported = f.take_events().iter().rev().find_map(|e| match e {
        EditorEvent::Scrolled { top_line, within } => Some((*top_line, *within)),
        _ => None,
    });
    let Some((line, within)) = reported else {
        panic!("no Scrolled event")
    };
    assert_eq!(line, 1);
    assert!(within > 0.5, "moved further into the line, got {within}");
}
