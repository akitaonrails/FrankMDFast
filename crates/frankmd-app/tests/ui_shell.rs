//! Adapted UI tests: shell layout, width classes, focus cycling, zen mode.
//!
//! Executable examples for docs/testing/ui-test-plan.md. Level-a tests target
//! the GUI-free view models of frankmd-workspace (`support::vm`); level-b
//! tests drive the real shell headlessly (`support::shell`).
#![allow(missing_docs, clippy::unwrap_used)]

extern crate gpui_kit as gpui;

mod support;

use gpui_kit::TestAppContext;
use support::harness::{self, MEDIUM, NARROW, WIDE};
use support::shell::{VaultFixture, open_shell};
use support::vm::{FocusRegion, LayoutState, PreviewMode, SidebarMode, WidthClass};

// ---- level a: frankmd_workspace::layout -----------------------------------

// adapts: test/system/workspace_test.rb:538 "long names and translated chrome fit desktop tablet and phone layouts"
// (the width-class part; ui-spec §4.1 replaces CSS breakpoints with classes + 24 px hysteresis)
#[test]
fn width_class_has_24px_hysteresis() {
    let mut layout = LayoutState::new();
    assert_eq!(layout.on_resize(1280.), WidthClass::Wide);
    assert_eq!(
        layout.on_resize(1090.),
        WidthClass::Wide,
        "within 24 px below 1100 stays wide"
    );
    assert_eq!(layout.on_resize(1070.), WidthClass::Medium);
    assert_eq!(
        layout.on_resize(1110.),
        WidthClass::Medium,
        "within 24 px above 1100 stays medium"
    );
    assert_eq!(layout.on_resize(1130.), WidthClass::Wide);
    assert_eq!(layout.on_resize(690.), WidthClass::Narrow);
}

// adapts: test/javascript/controllers/app_view_mode.test.js:137 "single mode: the preview toggle switches to the full-width preview pane"
// and :154 "single mode: toggling again returns to the editor pane and focuses it" (§4.2)
#[test]
fn narrow_forces_single_pane_swap() {
    let mut layout = LayoutState::new();
    layout.on_resize(640.);
    let a = layout.arrangement(640.);
    assert_eq!(
        a.sidebar,
        SidebarMode::Hidden,
        "narrow: sidebar only as drawer when opened"
    );
    assert!(a.editor);
    assert_eq!(a.preview, PreviewMode::Hidden);
    layout.toggle_preview();
    let a = layout.arrangement(640.);
    assert_eq!(a.preview, PreviewMode::Swapped);
    assert!(!a.editor);
    layout.toggle_preview();
    assert!(layout.arrangement(640.).editor);
}

// adapts: test/system/workspace_test.rb:402 "hidden Explorer reopens for Library and Settings and preserves preview mode"
// (§4.2: leaving the media library restores the previous preview visibility)
#[test]
fn media_library_restores_preview_visibility() {
    let mut layout = LayoutState::new();
    layout.toggle_preview(); // hide
    assert!(!layout.preview_visible);
    layout.toggle_media_library();
    layout.toggle_media_library();
    assert!(
        !layout.preview_visible,
        "preview stays hidden after leaving the library"
    );
}

// new behaviour, ui-spec §4.2 Zen (no FrankMD original; covers the zen rows of the plan)
#[test]
fn zen_hides_chrome_and_combines_with_typewriter() {
    let mut layout = LayoutState::new();
    layout.typewriter = true;
    layout.toggle_zen();
    let a = layout.arrangement(1280.);
    assert_eq!(a.sidebar, SidebarMode::Hidden);
    assert_eq!(a.preview, PreviewMode::Hidden);
    assert!(!a.pane_header);
    assert!(a.status_compact);
    assert!(layout.typewriter, "zen keeps typewriter on");
    layout.toggle_zen();
    let a = layout.arrangement(1280.);
    assert_eq!(a.sidebar, SidebarMode::Docked);
    assert_eq!(
        a.preview,
        PreviewMode::Split,
        "zen restores the previous arrangement"
    );
}

// ---- level a: frankmd_workspace::focus ------------------------------------

// adapts: test/javascript/controllers/app_editor_focus.test.js (it.each ×3) — focus lands in the
// editor after navigation; ui-spec §4.3 adds F6 region cycling that skips hidden regions.
#[test]
fn f6_cycle_skips_hidden_regions() {
    let mut layout = LayoutState::new();
    let all = layout.arrangement(1280.);
    assert_eq!(FocusRegion::Sidebar.next(&all), FocusRegion::Editor);
    assert_eq!(FocusRegion::Editor.next(&all), FocusRegion::Preview);
    assert_eq!(FocusRegion::Preview.next(&all), FocusRegion::Sidebar);
    assert_eq!(FocusRegion::Sidebar.prev(&all), FocusRegion::Preview);
    layout.toggle_preview();
    let no_preview = layout.arrangement(1280.);
    assert_eq!(FocusRegion::Editor.next(&no_preview), FocusRegion::Sidebar);
}

// ---- level b: the real shell, headless --------------------------------------

// adapts: test/system/notes_test.rb:6 "visiting the home page shows the app" and
// :164 "toggle preview panel" (§3.1 wide layout; §4.2 Ctrl+Shift+V)
#[gpui_kit::test]
fn wide_shell_shows_three_regions_and_preview_toggles(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    for id in [
        "sidebar",
        "editor-pane",
        "pane-header",
        "preview-pane",
        "status-line",
    ] {
        assert!(
            harness::is_shown(cx, w, id),
            "{id} visible in the wide layout"
        );
    }
    harness::press(cx, w, "ctrl-shift-v");
    assert!(!harness::is_shown(cx, w, "preview-pane"));
    assert!(!shell.layout(cx).preview_visible);
    harness::press(cx, w, "ctrl-shift-v");
    assert!(harness::is_shown(cx, w, "preview-pane"));
}

// adapts: test/system/workspace_test.rb:596 "mobile Explorer overlays the document and closes after selection and Escape"
// and :630 "resizing from mobile restores focus to the visible sidebar toggle" (§4.1 narrow drawer)
#[gpui_kit::test]
fn narrow_sidebar_is_a_drawer_closed_by_escape(cx: &mut TestAppContext) {
    let shell = open_shell(cx, NARROW, VaultFixture::sample());
    let w = shell.window;
    assert!(!harness::is_shown(cx, w, "sidebar"));
    harness::press(cx, w, "secondary-e");
    assert!(harness::is_shown(cx, w, "sidebar"));
    assert_eq!(
        shell.focus_region(cx),
        Some(FocusRegion::Sidebar),
        "drawer traps focus"
    );
    harness::press(cx, w, "escape");
    assert!(!harness::is_shown(cx, w, "sidebar"));
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
    harness::resize(cx, w, 1280.);
    assert!(
        harness::is_shown(cx, w, "sidebar"),
        "docked again at wide width"
    );
}

// §4.1 medium: sidebar auto-hides when the preview opens and the editor would drop below 320 px.
#[gpui_kit::test]
fn medium_width_auto_hides_sidebar_for_preview(cx: &mut TestAppContext) {
    // 248 (sidebar) + 320 (editor min) + 280 (preview min) = 848 > 760.
    let shell = open_shell(cx, MEDIUM, VaultFixture::sample());
    let w = shell.window;
    assert!(
        harness::is_shown(cx, w, "sidebar"),
        "900 px: room for all three"
    );
    harness::resize(cx, w, 760.);
    assert!(harness::is_shown(cx, w, "preview-pane"));
    assert!(
        !harness::is_shown(cx, w, "sidebar"),
        "sidebar auto-hides for the preview"
    );
    assert!(
        shell.layout(cx).sidebar_visible,
        "the user's preference is not overwritten"
    );
    harness::press(cx, w, "ctrl-shift-v");
    assert!(
        harness::is_shown(cx, w, "sidebar"),
        "comes back once the preview closes"
    );
}

// §4.3: F6 / Shift+F6 cycle, Ctrl+1/2/3 jump; the focused region is observable.
#[gpui_kit::test]
fn focus_cycles_with_f6_and_jumps_with_ctrl_digits(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    harness::press(cx, w, "ctrl-2");
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Editor));
    harness::press(cx, w, "f6");
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Preview));
    harness::press(cx, w, "f6");
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Sidebar));
    harness::press(cx, w, "shift-f6");
    assert_eq!(shell.focus_region(cx), Some(FocusRegion::Preview));
    harness::press(cx, w, "ctrl-1");
    assert!(harness::has_focus(cx, w, "explorer-tree"));
}

// §4.2 Zen: F11 hides sidebar/preview/header, banners stay, Esc doesn't leave, F11 does.
#[gpui_kit::test]
fn zen_mode_toggles_with_f11_not_escape(cx: &mut TestAppContext) {
    let shell = open_shell(cx, WIDE, VaultFixture::sample());
    let w = shell.window;
    shell.simulate(cx, support::vm::BannerKind::ContentLoss);
    harness::press(cx, w, "f11");
    for id in ["sidebar", "preview-pane", "pane-header"] {
        assert!(!harness::is_shown(cx, w, id), "{id} hidden in zen");
    }
    assert!(
        harness::is_shown(cx, w, "banner-content-loss"),
        "banners still show in zen"
    );
    harness::press(cx, w, "escape");
    assert!(shell.layout(cx).zen, "Esc never leaves zen");
    harness::press(cx, w, "f11");
    assert!(harness::is_shown(cx, w, "sidebar"));
}
