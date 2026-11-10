//! Level-b tests for the shell's surfaces (docs/testing/ui-test-plan.md):
//! help, settings sheet, diagnostics, theme/language pickers, explorer,
//! narrow drawer, status line, quick open, startup and preview wiring.
#![allow(missing_docs, clippy::unwrap_used)]

extern crate gpui_kit as gpui;

mod support;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{TestAppContext, point, px};
use support::harness::{self, NARROW, WIDE};
use support::shell::{Shell, VaultFixture, open_shell};
use support::vm::{BannerKind, FocusRegion, OverlayKind, PaletteMode, SaveState};

fn click(cx: &mut TestAppContext, shell: &Shell, id: &'static str) {
    harness::in_window(cx, shell.window, |window, cx| window.click(id, cx));
}

/// Click the overlay scrim near its top-left corner (outside the dialog).
fn click_scrim(cx: &mut TestAppContext, shell: &Shell) {
    harness::in_window(cx, shell.window, |window, cx| {
        window.click_at("overlay-scrim", point(px(4.), px(4.)), cx)
    });
}

fn fed(shell: &Shell) -> String {
    std::fs::read_to_string(shell.root.join(".fed")).unwrap_or_default()
}

fn locale(cx: &mut TestAppContext) -> &'static str {
    cx.update(|cx| frankmd_app::ui::i18n::current(cx).locale().code())
}

// ---- help (§5.18) -------------------------------------------------------------

// adapts: test/javascript/controllers/help_controller.test.js:44 "shows the help dialog"
// and :52 "closes the help dialog" (changed §11: F1 toggles)
#[gpui_kit::test]
fn f1_toggles_help_and_restores_editor_focus(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2 f1");
    assert!(harness::is_shown(cx, w, "help-dialog"));
    assert!(harness::has_focus(cx, w, "help-dialog"));
    harness::press(cx, w, "f1");
    assert!(!harness::is_shown(cx, w, "help-dialog"));
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
}

// adapts: help_controller.test.js:61 "closes help dialog on Escape" and :70 "does nothing for other keys"
#[gpui_kit::test]
fn help_closes_on_escape_but_not_on_other_keys(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "f1 enter");
    assert!(
        harness::is_shown(cx, w, "help-dialog"),
        "Enter doesn't dismiss help"
    );
    harness::press(cx, w, "escape");
    assert_eq!(shell.overlay(cx), None);
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
}

// adapts: help_controller.test.js:83 "closes dialog when clicking on backdrop"
#[gpui_kit::test]
fn help_closes_on_backdrop_click(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    harness::press(cx, shell.window, "f1");
    click_scrim(cx, &shell);
    assert_eq!(shell.overlay(cx), None);
}

// §5.18: Ctrl+/ opens Help directly on the Keyboard tab, generated from the registry.
#[gpui_kit::test]
fn keyboard_shortcuts_tab_lists_registry_bindings(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    harness::press(cx, shell.window, "ctrl-/");
    assert_eq!(shell.overlay(cx), Some(OverlayKind::Help));
    assert!(harness::is_shown(
        cx,
        shell.window,
        "help-key-view.toggle_zen"
    ));
}

// ---- settings sheet (§5.16) ------------------------------------------------------

// adapts: test/javascript/controllers/log_viewer_controller.test.js:474 "closes the dialog"
// (changed §5.16: Ctrl+, toggles the settings sheet) and :504 "closes on Escape key"
#[gpui_kit::test]
fn settings_toggles_with_ctrl_comma_and_closes_on_escape(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2 ctrl-,");
    assert!(harness::is_shown(cx, w, "settings-sheet"));
    assert!(
        harness::has_focus(cx, w, "settings-search"),
        "search field focused"
    );
    harness::press(cx, w, "ctrl-,");
    assert!(!harness::is_shown(cx, w, "settings-sheet"));
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
    harness::press(cx, w, "ctrl-, escape");
    assert_eq!(shell.overlay(cx), None);
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
}

// adapts: log_viewer_controller.test.js:484 "closes on backdrop click" and :494
// "does not close when clicking inside dialog"; settings_controller.test.js:428
#[gpui_kit::test]
fn settings_ignores_clicks_inside_and_closes_on_backdrop(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    harness::press(cx, shell.window, "ctrl-,");
    click(cx, &shell, "settings-category-editor");
    assert_eq!(
        shell.overlay(cx),
        Some(OverlayKind::Settings),
        "inside click keeps it open"
    );
    assert!(harness::is_shown(cx, shell.window, "setting-row-vim"));
    click_scrim(cx, &shell);
    assert_eq!(shell.overlay(cx), None);
}

// adapts: log_viewer_controller.test.js:95 "shows the dialog" (changed §5.19: Settings › Diagnostics)
#[gpui_kit::test]
fn diagnostics_shortcut_opens_settings_on_config_sources(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    harness::press(cx, shell.window, "ctrl-shift-o");
    assert_eq!(shell.overlay(cx), Some(OverlayKind::Settings));
    assert!(harness::is_shown(cx, shell.window, "config-source-theme"));
}

// §5.16 (FE-029 changed): inside Settings only Esc and Ctrl+, work.
#[gpui_kit::test]
fn global_shortcuts_are_disabled_inside_settings(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    harness::press(cx, shell.window, "ctrl-, ctrl-shift-p f11");
    assert_eq!(shell.overlay(cx), Some(OverlayKind::Settings));
    assert!(!shell.layout(cx).zen);
}

// §5.16: a switch row writes its key to `.fed` and applies live.
#[gpui_kit::test]
fn settings_switch_writes_fed(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    harness::press(cx, shell.window, "ctrl-,");
    click(cx, &shell, "settings-category-preview");
    click(cx, &shell, "setting-scroll_sync");
    assert!(
        fed(&shell).contains("scroll_sync = false"),
        "{}",
        fed(&shell)
    );
}

// ---- pickers (§5.1 sub-modes) ------------------------------------------------------

// adapts: test/javascript/controllers/locale_controller.test.js:130 "toggles menu visibility"
// (changed §5.1: palette sub-mode) and :155 "hides menu after selection"
#[gpui_kit::test]
fn language_picker_commits_live_and_restores_focus(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2");
    cx.dispatch_action(w, frankmd_app::ui::actions::SelectLanguage);
    assert_eq!(
        shell.overlay(cx),
        Some(OverlayKind::Palette(PaletteMode::Language))
    );
    harness::type_text(cx, w, "ja");
    harness::press(cx, w, "enter");
    assert_eq!(shell.overlay(cx), None);
    assert_eq!(locale(cx), "ja", "switches without a reload");
    assert!(fed(&shell).contains("locale = ja"));
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
}

// adapts: locale_controller.test.js:308 "closes menu when clicking outside"
#[gpui_kit::test]
fn language_picker_scrim_click_keeps_locale(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    cx.dispatch_action(shell.window, frankmd_app::ui::actions::SelectLanguage);
    harness::press(cx, shell.window, "down down");
    click_scrim(cx, &shell);
    assert_eq!(shell.overlay(cx), None);
    assert_eq!(locale(cx), "en");
}

// adapts: test/javascript/controllers/theme_controller.test.js:367 "closes menu when clicking
// outside" (changed §5.1: arrowing previews live, Esc/backdrop reverts, Enter keeps)
#[gpui_kit::test]
fn theme_picker_previews_reverts_and_commits(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    let bg = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            gpui_kit::component::theme::Theme::global(cx)
                .colors
                .background
        })
    };
    let before = bg(cx);
    cx.dispatch_action(w, frankmd_app::ui::actions::SelectTheme);
    harness::type_text(cx, w, "solarized light");
    harness::press(cx, w, "down up");
    assert_ne!(bg(cx), before, "arrowing previews the theme");
    harness::press(cx, w, "escape");
    assert_eq!(bg(cx), before, "Esc reverts");
    cx.dispatch_action(w, frankmd_app::ui::actions::SelectTheme);
    harness::type_text(cx, w, "solarized light");
    harness::press(cx, w, "enter");
    assert!(
        fed(&shell).contains("theme = solarized-light"),
        "{}",
        fed(&shell)
    );
    assert_ne!(bg(cx), before);
}

// §5.1: with no note open, a note command matches only as "Unavailable"; Enter does nothing.
#[gpui_kit::test]
fn palette_unavailable_rows_do_not_run(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample_no_note());
    harness::press(cx, shell.window, "ctrl-shift-p");
    harness::type_text(cx, shell.window, "copy path");
    assert!(harness::is_shown(
        cx,
        shell.window,
        "palette-row-file.copy_path"
    ));
    harness::press(cx, shell.window, "enter");
    assert_eq!(
        shell.overlay(cx),
        Some(OverlayKind::Palette(PaletteMode::Commands))
    );
}

// ---- layout & explorer -------------------------------------------------------------------

// adapts: test/javascript/controllers/app_sidebar.test.js:298 "keeps editor focus when the
// Explorer is opened by a keyboard shortcut"
#[gpui_kit::test]
fn ctrl_e_reopens_sidebar_without_stealing_focus(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2 secondary-e");
    assert!(!harness::is_shown(cx, w, "sidebar"));
    assert!(
        fed(&shell).contains("sidebar_visible = false"),
        "persisted per vault"
    );
    harness::press(cx, w, "secondary-e");
    assert!(harness::is_shown(cx, w, "sidebar"));
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
}

// adapts: app_sidebar.test.js:348 "opens the mobile overlay, contains focus, and closes it with Escape"
// (scrim part) and workspace_test.rb:596 "...closes after selection"
#[gpui_kit::test]
fn narrow_drawer_closes_on_scrim_and_after_opening_a_note(cx: &mut TestAppContext) {
    let shell = open_shell(cx, NARROW, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "secondary-e");
    click(cx, &shell, "drawer-scrim");
    assert!(!harness::is_shown(cx, w, "sidebar"));
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
    harness::press(cx, w, "secondary-e");
    click(cx, &shell, "explorer-row-Inbox.md");
    assert_eq!(shell.active_note(cx).as_deref(), Some("Inbox.md"));
    assert!(
        !harness::is_shown(cx, w, "sidebar"),
        "drawer closes after opening a note"
    );
}

// adapts: test/system/notes_test.rb:33 "clicking a note opens it in the editor"
#[gpui_kit::test]
fn clicking_an_explorer_row_opens_the_note(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    click(cx, &shell, "explorer-row-projects");
    click(cx, &shell, "explorer-row-projects/Launch list.md");
    assert_eq!(
        shell.active_note(cx).as_deref(),
        Some("projects/Launch list.md")
    );
    // Fixed to the plan (ui-test-plan §6 notes_test.rb:33, OQ-3): the row
    // stays selected and focused, the editor doesn't steal focus
    // (test/system/notes_test.rb:33 asserts `.explorer-selected:focus`).
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Sidebar));
    assert!(shell.view.read_with(cx, |s, _| {
        s.explorer().is_selected("projects/Launch list.md")
    }));
}

// §5.14 keyboard: ↓ moves, → expands, Enter opens.
#[gpui_kit::test]
fn explorer_keyboard_navigation_opens_notes(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample_no_note());
    let w = shell.window;
    harness::press(cx, w, "ctrl-1");
    assert!(harness::has_focus(cx, w, "explorer-tree"));
    // rows: blog, projects, reference, Inbox.md (folders first)
    harness::press(cx, w, "down down right down enter");
    assert_eq!(
        shell.active_note(cx).as_deref(),
        Some("projects/Launch list.md")
    );
}

// §4.2: the media library replaces editor + preview; Esc returns.
#[gpui_kit::test]
fn media_library_replaces_panes_and_escape_returns(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2 ctrl-shift-m");
    assert!(harness::is_shown(cx, w, "media-library"));
    assert!(!harness::is_shown(cx, w, "preview-pane"));
    harness::press(cx, w, "escape");
    assert!(!harness::is_shown(cx, w, "media-library"));
    assert!(harness::is_shown(cx, w, "preview-pane"));
}

// ---- notes, status line, startup ------------------------------------------------------------

// §5.23: Ctrl+W closes the note and returns to the empty state with quick open focused.
#[gpui_kit::test]
fn ctrl_w_returns_to_the_empty_state(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    harness::press(cx, shell.window, "ctrl-2 ctrl-w");
    assert_eq!(shell.active_note(cx), None);
    assert!(harness::is_shown(cx, shell.window, "empty-state"));
    assert!(harness::has_focus(cx, shell.window, "quick-open-input"));
}

// §3.6: typing marks the note unsaved; Ctrl+S saves it to disk.
#[gpui_kit::test]
fn ctrl_s_saves_what_was_typed(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2");
    harness::type_text(cx, w, "Hi ");
    assert_eq!(shell.save_state(cx), SaveState::Unsaved);
    harness::press(cx, w, "ctrl-s");
    cx.run_until_parked();
    assert_eq!(shell.save_state(cx), SaveState::Saved);
    assert!(
        shell
            .read_note("blog/2026/Project brief.md")
            .unwrap()
            .starts_with("Hi # Project brief")
    );
}

// §3.6 / FE-025: "Copy path" puts the path on the clipboard and says so in the status line.
#[gpui_kit::test]
fn copy_path_from_the_palette(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    harness::press(cx, shell.window, "ctrl-2 ctrl-shift-p");
    harness::type_text(cx, shell.window, "copy path");
    harness::press(cx, shell.window, "enter");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("blog/2026/Project brief.md")
    );
}

// §5.2: arrows move the quick-open selection while the input keeps focus.
#[gpui_kit::test]
fn quick_open_arrows_move_selection(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    // Recents first, newest first: the note opened at startup, Launch list, Inbox.
    harness::press(cx, w, "ctrl-p down down enter");
    assert_eq!(shell.active_note(cx).as_deref(), Some("Inbox.md"));
    harness::press(cx, w, "ctrl-p up enter");
    assert_eq!(
        shell.active_note(cx).as_deref(),
        Some("reference/Glossary.md"),
        "↑ on the first row wraps to the last"
    );
}

// §5.22 "Note not found": a missing CLI note isn't created; quick open is prefilled.
#[gpui_kit::test]
fn missing_startup_note_prefills_quick_open(cx: &mut TestAppContext) {
    let fixture = VaultFixture {
        open: Some("Nope.md"),
        ..VaultFixture::sample()
    };
    let shell = open_shell(cx, WIDE, fixture);
    assert_eq!(shell.active_note(cx), None);
    assert!(shell.read_note("Nope.md").is_none(), "never created");
    assert!(harness::has_focus(cx, shell.window, "quick-open-input"));
}

// §5.12: "Save anyway" clears the content-loss banner and returns focus to the editor.
#[gpui_kit::test]
fn content_loss_save_anyway_clears_banner(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    shell.simulate(cx, BannerKind::ContentLoss);
    click(cx, &shell, "banner-content-loss-save-anyway");
    assert!(shell.banners(cx).is_empty());
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
}

// §5.21: a preview checkbox toggles the task through the editor (an edit, never a direct write).
#[gpui_kit::test]
fn preview_task_checkbox_edits_the_note(cx: &mut TestAppContext) {
    let fixture = VaultFixture {
        open: Some("projects/Launch list.md"),
        ..VaultFixture::sample()
    };
    let shell = open_shell(cx, WIDE, fixture);
    cx.run_until_parked();
    harness::in_window(cx, shell.window, |_, _| ());
    click(cx, &shell, "preview-task-3");
    assert_eq!(shell.save_state(cx), SaveState::Unsaved);
    let text = shell.view.read_with(cx, |s, cx| s.editor_text(cx));
    assert!(text.contains("- [x] ship"), "{text}");
    assert_eq!(
        shell.read_note("projects/Launch list.md").as_deref(),
        Some("# Launch list\n\n- [ ] ship\n"),
        "not written directly"
    );
}

// §6.2: Ctrl+. is the context action even though gpui-kit's editor binds it to
// code actions in a deeper key context; the shell takes it first.
#[gpui_kit::test]
fn ctrl_dot_in_the_editor_runs_the_context_action(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    harness::press(cx, shell.window, "ctrl-2 ctrl-.");
    let message = shell
        .view
        .read_with(cx, |s, _| s.status_message())
        .unwrap_or_default();
    assert!(message.contains("Context action"), "{message}");
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
    assert_eq!(shell.save_state(cx), SaveState::Clean, "no edit happened");
}
