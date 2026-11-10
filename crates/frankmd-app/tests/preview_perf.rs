//! Preview render timing for large notes (run explicitly, in release):
//! `cargo test -p frankmd-app --release --test preview_perf -- --ignored --nocapture`
#![allow(missing_docs, clippy::unwrap_used)]

extern crate gpui_kit as gpui;

use std::time::Instant;

use frankmd_app::preview::PreviewPane;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, Bounds, Point, TestAppContext, WindowBounds, WindowOptions, px, size,
};

/// A realistic mixed note of about `target` bytes.
fn note(target: usize) -> String {
    let chunk = "## Section heading\n\nA paragraph with **bold**, *italic*, `code`, a [link](https://example.com) \
and a [[Wiki Link]] plus inline math $e^{i\\pi}+1=0$ and some more words to make it wrap nicely across lines.\n\n\
- [ ] a task item\n- [x] a done item\n- plain item with ==mark==\n\n\
```rust\nfn main() {\n    let x = vec![1, 2, 3];\n    println!(\"{:?}\", x);\n}\n```\n\n\
| A | B | C |\n|---|:-:|--:|\n| 1 | 2 | 3 |\n| 4 | 5 | 6 |\n\n> A quote with *emphasis*.\n\n$$\\int_0^1 x^2\\,dx$$\n\n";
    let mut out = String::from("---\ntitle: Big note\n---\n\n# Big note\n\n");
    while out.len() < target {
        out.push_str(chunk);
    }
    out
}

fn measure(cx: &mut TestAppContext, label: &str, bytes: usize) {
    let src = note(bytes);
    let t = Instant::now();
    let doc = frankmd_markdown::render(&src, &frankmd_markdown::RenderOptions::default());
    let parse = t.elapsed();

    let (window, pane) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Point::default(),
            size: size(px(1280.), px(800.)),
        };
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| PreviewPane::new(window, cx)),
        )
        .unwrap()
    });
    let t = Instant::now();
    pane.update(cx, |p, cx| p.set_markdown(src.clone(), None, cx));
    cx.run_until_parked();
    let pipeline = t.elapsed();
    let t = Instant::now();
    cx.update_window(window, |_, w, cx| w.render_frame(cx))
        .unwrap();
    let first_frame = t.elapsed();
    let mid = doc.total_lines / 2;
    let t = Instant::now();
    pane.update(cx, |p, cx| p.scroll_to_source_line(mid, 0.0, cx));
    cx.update_window(window, |_, w, cx| w.render_frame(cx))
        .unwrap();
    let jump = t.elapsed();
    let t = Instant::now();
    let edited = format!("{src}\nOne more line.\n");
    pane.update(cx, |p, cx| p.set_markdown(edited, None, cx));
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| w.render_frame(cx))
        .unwrap();
    let rerender = t.elapsed();
    let rows = pane.read_with(cx, |p, _| p.row_count());
    println!(
        "{label}: {} KB, {} lines, {} blocks, {rows} rows | parse (no highlight) {parse:?} | full pipeline (parse, layout, math) {pipeline:?} \
         | first frame {first_frame:?} | jump to line {mid} + frame {jump:?} | edit re-render + frame {rerender:?}",
        src.len() / 1024,
        doc.total_lines,
        doc.blocks.len(),
    );
}

#[gpui_kit::test]
#[ignore = "timing; run in release with --ignored --nocapture"]
fn render_large_notes(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    measure(cx, "330 KB note", 330 * 1024);
    measure(cx, "5 MB note", 5 * 1024 * 1024);
}
