//! Headless GPUI tests for the preview pane (docs/testing/ui-test-plan.md,
//! preview-level "b" rows, plus the preview wiring of ui-spec §5.20/§5.21).
#![allow(missing_docs, clippy::unwrap_used)]

// `#[gpui_kit::test]` expands to `gpui::…` paths; alias the kit as `gpui`.
extern crate gpui_kit as gpui;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use frankmd_app::preview::{PreviewEvent, PreviewPane};
use frankmd_markdown::ir::{Block, Inline};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Point, ScrollDelta, TestAppContext,
    WindowBounds, WindowOptions, point, px, size,
};

type Events = Rc<RefCell<Vec<PreviewEvent>>>;

struct Fixture {
    window: AnyWindowHandle,
    pane: Entity<PreviewPane>,
    events: Events,
}

fn open(cx: &mut TestAppContext, width: f32, height: f32) -> Fixture {
    cx.update(gpui_kit::init);
    let (window, pane) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Point::default(),
            size: size(px(width), px(height)),
        };
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| PreviewPane::new(window, cx)),
        )
        .expect("open window")
    });
    let events: Events = Rc::default();
    let sink = events.clone();
    cx.update(|cx| {
        cx.subscribe(&pane, move |_, ev: &PreviewEvent, _| {
            sink.borrow_mut().push(ev.clone())
        })
        .detach();
    });
    Fixture {
        window,
        pane,
        events,
    }
}

impl Fixture {
    fn show(&self, cx: &mut TestAppContext, md: &str, base: Option<PathBuf>) {
        let md = md.to_owned();
        self.pane.update(cx, |p, cx| p.set_markdown(md, base, cx));
        cx.run_until_parked();
        self.frame(cx);
    }

    fn frame(&self, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }

    fn with<R>(
        &self,
        cx: &mut TestAppContext,
        f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R,
    ) -> R {
        cx.update_window(self.window, |_, window, cx| {
            window.render_frame(cx);
            f(window, cx)
        })
        .unwrap()
    }

    fn shown(&self, cx: &mut TestAppContext, id: &'static str) -> bool {
        self.with(cx, |w, _| w.try_find(id).is_some_and(|e| e.visible()))
    }

    fn focused(&self, cx: &mut TestAppContext, id: &'static str) -> bool {
        self.with(cx, |w, _| {
            w.try_find(id).and_then(|e| e.focused()) == Some(true)
        })
    }

    fn texts(&self, cx: &mut TestAppContext) -> Vec<String> {
        self.with(cx, |w, _| {
            gpui_kit::base::test_support::texts(w)
                .iter()
                .filter(|t| t.visible())
                .map(|t| t.text().to_owned())
                .collect()
        })
    }

    fn take_events(&self) -> Vec<PreviewEvent> {
        std::mem::take(&mut self.events.borrow_mut())
    }
}

#[allow(clippy::disallowed_methods)] // test fixture setup
fn vault() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("frankmd-preview-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("notes")).unwrap();
    dir.canonicalize().unwrap()
}

// adapts: test/system/editor_test.rb:164 "toggle preview panel" (preview part):
// the markdown renders natively (h1, strong) inside a visible `preview-pane`.
#[gpui_kit::test]
fn renders_heading_and_strong(cx: &mut TestAppContext) {
    let f = open(cx, 900., 700.);
    f.show(cx, "# Preview Test\n\nSome **bold** text\n", None);
    assert!(f.shown(cx, "preview-pane"));
    let (h1, strong) = f.pane.read_with(cx, |p, _| {
        let doc = p.document();
        let h1 = matches!(&doc.blocks[0], Block::Heading { level: 1, inlines, .. } if inlines == &[Inline::Text("Preview Test".into())]);
        let strong = matches!(&doc.blocks[1], Block::Paragraph { inlines, .. }
            if inlines.iter().any(|i| *i == Inline::Strong(vec![Inline::Text("bold".into())])));
        (h1, strong)
    });
    assert!(h1 && strong);
    let texts = f.texts(cx);
    assert!(texts.iter().any(|t| t == "Preview Test"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "Some bold text"), "{texts:?}");
}

// Front matter is hidden (FE-181) behind a disclosure row.
#[gpui_kit::test]
fn front_matter_is_hidden_behind_disclosure(cx: &mut TestAppContext) {
    let f = open(cx, 900., 700.);
    f.show(cx, "---\ntitle: Secret Title\n---\n\nBody\n", None);
    assert!(!f.texts(cx).iter().any(|t| t.contains("Secret Title")));
    assert!(f.shown(cx, "preview-front-matter"));
    f.with(cx, |w, cx| w.click("preview-front-matter", cx));
    f.frame(cx);
    // The raw block is plain text (not recorded); the row toggled open.
    assert!(f.texts(cx).iter().any(|t| t.starts_with("Front matter ▾")));
}

// adapts: test/system/preview_image_viewer_test.rb:6 "images open through keyboard
// and close through Escape with focus restored".
#[gpui_kit::test]
fn image_opens_with_enter_and_escape_restores_focus(cx: &mut TestAppContext) {
    let dir = vault();
    let f = open(cx, 1280., 800.);
    f.pane
        .update(cx, |p, cx| p.set_vault_root(Some(dir.clone()), cx));
    f.show(
        cx,
        "![Example](one.png)\n\n![Second](two.png)\n",
        Some(dir.join("notes")),
    );
    assert!(f.shown(cx, "preview-image-0"));
    f.with(cx, |w, cx| {
        f.pane.update(cx, |p, cx| p.focus_image(0, w, cx))
    });
    assert!(f.focused(cx, "preview-image-0"));
    f.with(cx, |w, cx| w.press("enter", cx));
    assert!(f.shown(cx, "image-viewer"));
    assert!(
        f.texts(cx).iter().any(|t| t == "Example  1/2"),
        "{:?}",
        f.texts(cx)
    );
    // Right moves to the next image.
    f.with(cx, |w, cx| w.press("right", cx));
    assert!(f.texts(cx).iter().any(|t| t == "Second  2/2"));
    f.with(cx, |w, cx| w.press("left", cx));
    f.with(cx, |w, cx| w.press("escape", cx));
    assert!(!f.shown(cx, "image-viewer"));
    assert!(f.focused(cx, "preview-image-0"));
    assert!(f.focused(cx, "preview-pane"));
}

// adapts: test/system/preview_image_viewer_test.rb:28 "linked image keeps navigation
// and its enlarge control fits mobile viewport" + image_viewer_controller.test.js:36.
#[gpui_kit::test]
fn linked_image_follows_link_and_enlarge_fits_phone(cx: &mut TestAppContext) {
    let dir = vault();
    let f = open(cx, 390., 844.);
    f.pane
        .update(cx, |p, cx| p.set_vault_root(Some(dir.clone()), cx));
    f.show(
        cx,
        "[![Linked](icon.png)](/notes/target.md)\n",
        Some(dir.join("notes")),
    );
    f.with(cx, |w, cx| w.click("preview-image-0", cx));
    cx.run_until_parked();
    assert_eq!(
        f.take_events(),
        vec![PreviewEvent::OpenNote {
            path: dir.join("target.md")
        }]
    );
    assert!(!f.shown(cx, "image-viewer"));
    f.with(cx, |w, cx| w.click("preview-image-enlarge-0", cx));
    assert!(f.shown(cx, "image-viewer"));
    let bounds = f.with(cx, |w, _| w.find("image-viewer").bounds());
    assert!(bounds.origin.x >= px(0.) && bounds.origin.y >= px(0.));
    assert!(bounds.size.width <= px(390.) && bounds.size.height <= px(844.));
    // Clicking the backdrop closes it.
    f.with(cx, |w, cx| {
        w.click_at("image-viewer", point(px(4.), px(4.)), cx)
    });
    assert!(!f.shown(cx, "image-viewer"));
}

// FE-197 / ui-spec §5.21 Task lists: clicking a checkbox asks to toggle that source line.
#[gpui_kit::test]
fn task_checkbox_emits_toggle_for_true_source_line(cx: &mut TestAppContext) {
    let f = open(cx, 900., 700.);
    f.show(
        cx,
        "---\ntitle: x\n---\n\n# Tasks\n\n- [ ] first\n- [x] second\n",
        None,
    );
    assert!(f.shown(cx, "preview-task-7"));
    f.with(cx, |w, cx| w.click("preview-task-8", cx));
    cx.run_until_parked();
    assert_eq!(
        f.take_events(),
        vec![PreviewEvent::ToggleTask {
            line: 8,
            checked: true
        }]
    );
}

// FE-212…217: scroll sync uses the IR's true source lines in both directions.
#[gpui_kit::test]
fn sync_scroll_maps_source_lines_both_ways(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    let md: String = (1..=400)
        .map(|i| format!("Paragraph number {i}\n\n"))
        .collect();
    f.show(cx, &md, None);
    f.pane
        .update(cx, |p, cx| p.scroll_to_source_line(201, 0.0, cx));
    f.frame(cx);
    assert_eq!(
        f.pane.read_with(cx, |p, _| p.visible_source_line()),
        Some(201)
    );
    // Programmatic scrolls do not echo back to the editor.
    cx.run_until_parked();
    assert!(f.take_events().is_empty());
    // A user scroll reports the new top line.
    f.with(cx, |w, cx| {
        w.scroll(
            "preview-pane",
            ScrollDelta::Pixels(point(px(0.), px(-300.))),
            cx,
        )
    });
    cx.run_until_parked();
    let events = f.take_events();
    let Some(PreviewEvent::VisibleSourceLine(line, _)) = events.last() else {
        panic!("no sync event: {events:?}")
    };
    assert!(*line > 201, "scrolled down from 201, got {line}");
    assert_eq!(*line % 2, 1, "paragraphs sit on odd lines");
}

// A scroll requested before the render lands is applied once it does.
#[gpui_kit::test]
fn scroll_before_render_is_deferred(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    let md: String = (1..=100).map(|i| format!("Line {i}\n\n")).collect();
    f.pane.update(cx, |p, cx| {
        p.set_markdown(md, None, cx);
        p.scroll_to_source_line(51, 0.0, cx);
    });
    cx.run_until_parked();
    f.frame(cx);
    assert_eq!(
        f.pane.read_with(cx, |p, _| p.visible_source_line()),
        Some(51)
    );
}

// FE-014/219: zoom clamps to 50–200 % and announces changes made in the pane.
#[gpui_kit::test]
fn zoom_clamps_and_announces(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    f.show(cx, "# Zoom\n", None);
    f.pane.update(cx, |p, cx| p.set_zoom(150, cx));
    assert_eq!(f.pane.read_with(cx, |p, _| p.zoom()), 150);
    f.pane.update(cx, |p, cx| p.set_zoom(1000, cx));
    assert_eq!(f.pane.read_with(cx, |p, _| p.zoom()), 200);
    f.pane.update(cx, |p, cx| p.zoom_out(cx));
    assert_eq!(f.take_events(), vec![PreviewEvent::ZoomChanged(190)]);
    let small = f.with(cx, |w, _| w.find("preview-pane").bounds());
    assert!(small.size.width > px(0.));
}

// SECURITY: raw HTML is shown as escaped text, never interpreted.
#[gpui_kit::test]
fn raw_html_renders_as_text(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    f.show(
        cx,
        "<script>alert(1)</script>\n\nInline <b onclick=\"x()\">bold</b> here\n",
        None,
    );
    let texts = f.texts(cx);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("<script>alert(1)</script>")),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("<b onclick=\"x()\">")),
        "{texts:?}"
    );
}

// SECURITY: only http(s)/mailto links reach the system opener.
#[gpui_kit::test]
fn external_links_open_only_for_web_schemes(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    f.show(cx, "[site](https://example.com/page)\n", None);
    f.with(cx, |w, cx| {
        w.click_at("preview-link-0-1", point(px(6.), px(6.)), cx)
    });
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/page"));
}

#[gpui_kit::test]
fn file_links_are_never_opened(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    f.show(cx, "[secret](file:///etc/passwd)\n", None);
    // The IR drops the href (DOMPurify allowlist), so the text is not even a link.
    assert!(f.with(cx, |w, _| w.try_find("preview-link-0-1").is_none()));
    assert_eq!(cx.opened_url(), None);
}

// Wikilinks ask the shell to open the note.
#[gpui_kit::test]
fn wikilink_click_emits_open(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    f.show(cx, "[[projects/Plan|the plan]]\n", None);
    assert!(f.texts(cx).iter().any(|t| t == "the plan"));
    f.with(cx, |w, cx| {
        w.click_at("preview-link-0-1", point(px(6.), px(6.)), cx)
    });
    cx.run_until_parked();
    assert_eq!(
        f.take_events(),
        vec![PreviewEvent::OpenWikiLink {
            target: "projects/Plan".into()
        }]
    );
}

// FE-201/203: the copy button is always visible and copies the block's code.
#[gpui_kit::test]
fn code_block_copy_button(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    f.show(cx, "```rust\nfn main() {}\nlet x = 1;\n```\n", None);
    let texts = f.texts(cx);
    assert!(
        texts.iter().any(|t| t == "Rust"),
        "language label: {texts:?}"
    );
    assert!(texts.iter().any(|t| t == "fn main() {}\nlet x = 1;"));
    assert!(f.shown(cx, "preview-code-copy-0"));
    f.with(cx, |w, cx| w.click("preview-code-copy-0", cx));
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("fn main() {}\nlet x = 1;")
    );
    assert!(f.texts(cx).iter().any(|t| t == "Copied"));
}

// Math renders through RaTeX as an image; errors show the source.
#[gpui_kit::test]
fn math_renders_and_errors_show_source(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    f.show(
        cx,
        "Inline $x^2$ math\n\n$$\\frac{a}{b}$$\n\nBad $\\href{javascript:alert(1)}{x}$ end\n",
        None,
    );
    let texts = f.texts(cx);
    assert!(
        texts.iter().any(|t| t == "x^2"),
        "inline math alt: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "\\frac{a}{b}"),
        "display math alt: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t == "$\\href{javascript:alert(1)}{x}$"),
        "error source: {texts:?}"
    );
}

// YouTube embeds are thumbnail cards that open the browser (no webview).
#[gpui_kit::test]
fn youtube_embed_is_a_card(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    f.show(
        cx,
        "<iframe src=\"https://www.youtube.com/embed/dQw4w9WgXcQ\"></iframe>\n",
        None,
    );
    assert!(f.shown(cx, "preview-embed-dQw4w9WgXcQ"));
    f.with(cx, |w, cx| w.click("preview-embed-dQw4w9WgXcQ", cx));
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
    );
}

// ui-spec §5.22: preview of a non-markdown file.
#[gpui_kit::test]
fn non_markdown_message(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    f.pane.update(cx, |p, cx| p.set_not_markdown(cx));
    cx.run_until_parked();
    assert!(
        f.texts(cx)
            .iter()
            .any(|t| t == "Preview is available for Markdown notes.")
    );
}

// Only the newest render lands (generation guard).
#[gpui_kit::test]
fn stale_renders_are_dropped(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    f.pane.update(cx, |p, cx| {
        p.set_markdown("# Old\n", None, cx);
        p.set_markdown("# New\n", None, cx);
    });
    cx.run_until_parked();
    let texts = f.texts(cx);
    assert!(texts.iter().any(|t| t == "New"));
    assert!(!texts.iter().any(|t| t == "Old"));
    assert!(f.pane.read_with(cx, |p, _| p.is_up_to_date()));
}

// Huge blocks are chunked so the list stays virtual.
#[gpui_kit::test]
fn long_code_block_is_virtualized(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    let body: String = (0..5000).map(|i| format!("line {i}\n")).collect();
    f.show(cx, &format!("```\n{body}```\n"), None);
    let rows = f.pane.read_with(cx, |p, _| p.row_count());
    assert!(rows > 40, "chunked into {rows} rows");
    // Only the first chunk(s) are laid out.
    let painted = f
        .texts(cx)
        .iter()
        .filter(|t| t.starts_with("line "))
        .count();
    assert!(painted <= 4, "{painted} code chunks painted");
}

// Scrolling inside one long paragraph reports how far into its source line the
// top is, so the editor follows smoothly instead of jumping a line at a time.
#[gpui_kit::test]
fn user_scroll_inside_a_long_paragraph_reports_progress(cx: &mut TestAppContext) {
    let f = open(cx, 900., 600.);
    let md = format!("Intro\n\n{}\n\nEnd\n", "word ".repeat(6000));
    f.show(cx, &md, None);
    f.pane
        .update(cx, |p, cx| p.scroll_to_source_line(3, 0.0, cx));
    f.frame(cx);
    cx.run_until_parked();
    f.take_events();
    f.with(cx, |w, cx| {
        w.scroll(
            "preview-pane",
            ScrollDelta::Pixels(point(px(0.), px(-200.))),
            cx,
        )
    });
    cx.run_until_parked();
    let events = f.take_events();
    let Some(PreviewEvent::VisibleSourceLine(line, within)) = events.last() else {
        panic!("no sync event: {events:?}")
    };
    assert_eq!(*line, 3);
    assert!(*within > 0.0, "no progress inside the paragraph");
}
