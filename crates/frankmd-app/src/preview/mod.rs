//! Preview pane: renders the frankmd-markdown IR as native GPUI elements
//! (ui-spec §3.5, §5.20, §5.21; docs/research/gpui-stack.md §4).
//!
//! - Parsing, highlighting and math layout run on the background executor,
//!   tagged with a generation; stale results are dropped.
//! - The document is a virtual `list()` over [`items`] rows (huge blocks are
//!   chunked), so a 5 MB note lays out only what is on screen.
//! - Sync scroll uses the IR's true source lines: [`PreviewEvent::VisibleSourceLine`]
//!   on user scroll, [`PreviewPane::scroll_to_source_line`] from the editor.
//! - SECURITY: raw HTML is shown as escaped text; images load only from the
//!   vault or `https` (egress client); links open externally only for
//!   `http(s)`/`mailto` (see [`links`]).

pub mod inline;
pub mod items;
pub mod links;
pub mod math;
pub mod remote;
mod render;
pub mod style;
mod viewer;

use crate::scale::px;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use frankmd_i18n::{Catalog, Locale};
use frankmd_markdown::highlight::{BuiltinHighlighter, CodeHighlighter};
use frankmd_markdown::{RenderDoc, RenderOptions};
use gpui_kit::{
    App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, ListAlignment,
    ListOffset, ListState, SharedString, Task, Window,
};

pub use links::{ImageSource, LinkTarget};
pub use style::{DEFAULT_ZOOM, MAX_ZOOM, MIN_ZOOM, PreviewColors, ZOOM_STEP, clamp_zoom, text_px};

use inline::LinkRef;
use items::Layout;

/// Events the preview emits for the shell to act on.
#[derive(Debug, Clone, PartialEq)]
pub enum PreviewEvent {
    /// The user scrolled the preview; this source line is at its top, the second
    /// field (`[0, 1)`) of the way into it (sync scroll).
    VisibleSourceLine(u32, f32),
    /// A task checkbox was clicked: toggle the `[ ]`/`[x]` marker on this
    /// 1-based source line (`frankmd_core::text::task::task_marker_range`).
    ToggleTask {
        /// 1-based line of the whole note.
        line: u32,
        /// State shown before the click.
        checked: bool,
    },
    /// A `[[wikilink]]` was clicked.
    OpenWikiLink {
        /// The link target as written.
        target: String,
    },
    /// A relative link to another note was clicked (absolute, inside the vault).
    OpenNote {
        /// Absolute path.
        path: PathBuf,
    },
    /// Zoom changed from inside the pane (header buttons, keys).
    ZoomChanged(u16),
}

/// How long the "Copied" feedback of a code block's copy button stays.
const COPIED_FEEDBACK: Duration = Duration::from_millis(1500);
/// Overdraw of the virtual list (px rendered beyond the viewport).
const LIST_OVERDRAW: f32 = 600.0;
/// Default text column width (`editor_width`, ch).
const DEFAULT_WIDTH_CH: u16 = 80;

/// The preview pane view.
pub struct PreviewPane {
    focus: FocusHandle,
    source: Arc<str>,
    doc: Arc<RenderDoc>,
    layout: Arc<Layout>,
    generation: u64,
    rendered_generation: u64,
    render_task: Option<Task<()>>,
    list: ListState,
    colors: PreviewColors,
    zoom: u16,
    width_ch: u16,
    is_markdown: bool,
    link_cx: links::Context,
    catalog: Arc<Catalog>,
    math: math::MathCache,
    remote: remote::RemoteImages,
    image_focus: Vec<FocusHandle>,
    viewer: Option<viewer::Viewer>,
    show_front_matter: bool,
    copied: Option<usize>,
    copied_task: Option<Task<()>>,
    pending_line: Option<(u32, f32)>,
    last_reported_line: Option<(u32, f32)>,
    highlighter: Arc<dyn CodeHighlighter>,
}

impl EventEmitter<PreviewEvent> for PreviewPane {}

impl Focusable for PreviewPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl PreviewPane {
    /// Create an empty preview.
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(LIST_OVERDRAW));
        let weak = cx.entity().downgrade();
        list.set_scroll_handler(move |_, _, cx| {
            // The list state is borrowed while this runs: report on the next tick.
            let weak = weak.clone();
            cx.defer(move |cx| {
                weak.update(cx, |this, cx| this.report_visible_line(cx))
                    .ok();
            });
        });
        let dark = gpui_kit::component::ActiveTheme::theme(&**cx)
            .mode
            .is_dark();
        Self {
            focus: cx.focus_handle(),
            source: Arc::from(""),
            doc: Arc::new(RenderDoc::default()),
            layout: Arc::new(Layout::default()),
            generation: 0,
            rendered_generation: 0,
            render_task: None,
            list,
            colors: PreviewColors::builtin(dark),
            zoom: DEFAULT_ZOOM,
            width_ch: DEFAULT_WIDTH_CH,
            is_markdown: true,
            link_cx: links::Context::default(),
            catalog: Arc::new(Catalog::load(Locale::En).unwrap_or_default()),
            math: math::MathCache::default(),
            remote: remote::RemoteImages::default(),
            image_focus: Vec::new(),
            viewer: None,
            show_front_matter: false,
            copied: None,
            copied_task: None,
            pending_line: None,
            last_reported_line: None,
            highlighter: Arc::new(BuiltinHighlighter),
        }
    }

    /// Show `source` (a whole note, front matter included). `base_dir` is the
    /// note's folder (relative links and images resolve there). Rendering
    /// happens off the main thread; older pending renders are dropped.
    pub fn set_markdown(
        &mut self,
        source: impl Into<Arc<str>>,
        base_dir: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let source: Arc<str> = source.into();
        self.is_markdown = true;
        if self.link_cx.base_dir != base_dir {
            self.link_cx.base_dir = base_dir;
        }
        if *source == *self.source
            && self.rendered_generation == self.generation
            && self.generation > 0
        {
            return;
        }
        self.source = source.clone();
        self.generation += 1;
        let generation = self.generation;
        let opts = RenderOptions {
            highlighter: Some(self.highlighter.clone()),
            ..RenderOptions::default()
        };
        let layouts = self.math.shared();
        let job = cx.background_executor().spawn(async move {
            let doc = frankmd_markdown::render(&source, &opts);
            let layout = items::layout(&doc);
            math::prepare(&doc, &layouts);
            (doc, layout)
        });
        self.render_task = Some(cx.spawn(async move |this, cx| {
            let (doc, layout) = job.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.install(doc, layout, generation, cx);
            })
            .ok();
        }));
    }

    /// Show the "Preview is available for Markdown notes." state (non-markdown file).
    pub fn set_not_markdown(&mut self, cx: &mut Context<Self>) {
        self.is_markdown = false;
        self.generation += 1;
        self.render_task = None;
        self.source = Arc::from("");
        self.install(RenderDoc::default(), Layout::default(), self.generation, cx);
    }

    fn install(&mut self, doc: RenderDoc, layout: Layout, generation: u64, cx: &mut Context<Self>) {
        let old_top = self.list.logical_scroll_top();
        let first_render = self.rendered_generation == 0;
        let old_count = self.list.item_count();
        let new_count = layout.items.len();
        self.doc = Arc::new(doc);
        self.layout = Arc::new(layout);
        self.rendered_generation = generation;
        // Viewer and focus slots refer to images by ordinal; keep them valid.
        if self.image_focus.len() > self.layout.images.len() {
            self.image_focus.truncate(self.layout.images.len());
        }
        if let Some(v) = &self.viewer
            && v.index >= self.layout.images.len()
        {
            self.viewer = None;
        }
        self.list.splice(0..old_count, new_count);
        if !first_render && new_count > 0 {
            let item_ix = old_top.item_ix.min(new_count - 1);
            self.list.scroll_to(ListOffset {
                item_ix,
                offset_in_item: old_top.offset_in_item,
            });
        }
        if let Some((line, within)) = self.pending_line.take() {
            self.scroll_to_source_line(line, within, cx);
        }
        cx.notify();
    }

    /// Set the vault root (containment boundary for images and note links).
    pub fn set_vault_root(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        self.link_cx.vault_root = root;
        cx.notify();
    }

    /// Apply theme tokens (Omarchy / built-in / custom).
    pub fn set_theme(&mut self, tokens: &frankmd_theme::UiTokens, cx: &mut Context<Self>) {
        self.colors = PreviewColors::from_tokens(tokens);
        self.math.clear_images();
        cx.notify();
    }

    /// Set the UI language.
    pub fn set_catalog(&mut self, catalog: Arc<Catalog>, cx: &mut Context<Self>) {
        self.catalog = catalog;
        cx.notify();
    }

    /// Text column width in `ch` (Settings › Editor › Text width).
    pub fn set_text_width(&mut self, width_ch: u16, cx: &mut Context<Self>) {
        self.width_ch = width_ch.clamp(40, 200);
        self.list.remeasure();
        cx.notify();
    }

    /// Set zoom in percent (clamped to 50–200).
    pub fn set_zoom(&mut self, percent: u16, cx: &mut Context<Self>) {
        let zoom = clamp_zoom(percent);
        if zoom != self.zoom {
            self.zoom = zoom;
            self.list.remeasure();
            cx.notify();
        }
    }

    /// Current zoom in percent.
    pub fn zoom(&self) -> u16 {
        self.zoom
    }

    /// Zoom in one step and announce it.
    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(self.zoom.saturating_add(ZOOM_STEP), cx);
        cx.emit(PreviewEvent::ZoomChanged(self.zoom));
    }

    /// Zoom out one step and announce it.
    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(self.zoom.saturating_sub(ZOOM_STEP), cx);
        cx.emit(PreviewEvent::ZoomChanged(self.zoom));
    }

    /// Reset zoom to 100 % and announce it.
    pub fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(DEFAULT_ZOOM, cx);
        cx.emit(PreviewEvent::ZoomChanged(self.zoom));
    }

    /// Scroll so source `line` (1-based, whole note), `within` (`[0, 1)`) of the
    /// way into it, is at the top. Applied after the pending render when called
    /// before it lands. Does not echo a [`PreviewEvent::VisibleSourceLine`].
    pub fn scroll_to_source_line(&mut self, line: u32, within: f32, cx: &mut Context<Self>) {
        if self.rendered_generation != self.generation || self.layout.items.is_empty() {
            self.pending_line = Some((line, within));
            return;
        }
        let ix = items::item_for_line(&self.layout.items, line);
        let item = &self.layout.items[ix];
        let height = self
            .list
            .bounds_for_item(ix)
            .map(|b| b.size.height)
            .unwrap_or_default();
        let offset = height * items::fraction_in_item(item, line, within);
        self.list.scroll_to(ListOffset {
            item_ix: ix,
            offset_in_item: offset,
        });
        self.last_reported_line = Some((line, within));
        cx.notify();
    }

    /// Source line currently at the top of the viewport.
    pub fn visible_source_line(&self) -> Option<u32> {
        self.visible_position().map(|(line, _)| line)
    }

    /// Source line at the top of the viewport, and how far (`[0, 1)`) into it.
    fn visible_position(&self) -> Option<(u32, f32)> {
        let top = self.list.logical_scroll_top();
        let item = self.layout.items.get(top.item_ix)?;
        let height = self
            .list
            .bounds_for_item(top.item_ix)
            .map(|b| b.size.height);
        let fraction = match height {
            Some(h) if h > px(0.) => (top.offset_in_item / h).clamp(0.0, 1.0),
            _ => 0.0,
        };
        Some(items::line_in_item(item, fraction))
    }

    fn report_visible_line(&mut self, cx: &mut Context<Self>) {
        let Some((line, within)) = self.visible_position() else {
            return;
        };
        if self.last_reported_line != Some((line, within)) {
            self.last_reported_line = Some((line, within));
            cx.emit(PreviewEvent::VisibleSourceLine(line, within));
        }
    }

    /// The rendered document (tests, status line word counts…).
    pub fn document(&self) -> &RenderDoc {
        &self.doc
    }

    /// Whether the last `set_markdown` has been rendered.
    pub fn is_up_to_date(&self) -> bool {
        self.rendered_generation == self.generation
    }

    /// Number of virtual-list rows.
    pub fn row_count(&self) -> usize {
        self.layout.items.len()
    }

    /// Move keyboard focus to image `ordinal` (document order), e.g. for the
    /// "View image" context chip; Enter then opens the viewer.
    pub fn focus_image(&mut self, ordinal: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ordinal < self.layout.images.len() {
            self.image_focus(ordinal, cx).focus(window, cx);
            cx.notify();
        }
    }

    /// Open the image viewer on image `ordinal` (document order).
    pub fn view_image(&mut self, ordinal: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.open_viewer(ordinal, window, cx);
    }

    /// Whether the image viewer (lightbox) is open.
    pub fn is_viewer_open(&self) -> bool {
        self.viewer.is_some()
    }

    /// Localized string with an English fallback for keys not yet in the catalogs.
    fn t(&self, key: &str, fallback: &str) -> SharedString {
        let s = self.catalog.t(key, &[]);
        if s == key {
            SharedString::from(fallback.to_owned())
        } else {
            s.into()
        }
    }

    /// Handle a click on a link range.
    fn activate_link(&mut self, link: &LinkRef, _window: &mut Window, cx: &mut Context<Self>) {
        match link {
            LinkRef::Wiki(target) => cx.emit(PreviewEvent::OpenWikiLink {
                target: target.clone(),
            }),
            LinkRef::Url(href) => match links::classify_link(href, &self.link_cx) {
                LinkTarget::External(url) => cx.open_url(&url),
                LinkTarget::Anchor(anchor) => self.scroll_to_anchor(&anchor, cx),
                LinkTarget::Note(path) => cx.emit(PreviewEvent::OpenNote { path }),
                LinkTarget::Blocked => tracing::debug!("preview: link not followed"),
            },
        }
    }

    fn scroll_to_anchor(&mut self, anchor: &str, cx: &mut Context<Self>) {
        if let Some((_, ix)) = self.layout.anchors.iter().find(|(a, _)| a == anchor) {
            self.list.scroll_to(ListOffset {
                item_ix: *ix,
                offset_in_item: px(0.),
            });
            self.report_visible_line(cx);
            cx.notify();
        }
    }

    /// A task checkbox was clicked. Guarded against a stale render: the
    /// source line must still be a task line.
    fn toggle_task(&mut self, line: u32, checked: bool, cx: &mut Context<Self>) {
        let text = self
            .source
            .split('\n')
            .nth(line.saturating_sub(1) as usize)
            .unwrap_or_default();
        if frankmd_core::text::task::task_marker_range(text).is_some() {
            cx.emit(PreviewEvent::ToggleTask { line, checked });
        }
    }

    fn copy_code(&mut self, ordinal: usize, code: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(code));
        self.copied = Some(ordinal);
        self.copied_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FEEDBACK).await;
            this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Focus slot of image `ordinal`, created on demand.
    fn image_focus(&mut self, ordinal: usize, cx: &mut Context<Self>) -> FocusHandle {
        while self.image_focus.len() <= ordinal {
            self.image_focus.push(cx.focus_handle());
        }
        self.image_focus[ordinal].clone()
    }

    /// Start fetching a remote image through the egress client.
    fn fetch_remote(&mut self, url: &str, cx: &mut Context<Self>) {
        if !self.remote.begin(url) {
            return;
        }
        let client = remote::client().cloned();
        let has_runtime = cx.has_global::<crate::bridge::tokio::NetRuntimeGlobal>();
        let (Some(client), true) = (client, has_runtime) else {
            self.remote.finish(url, remote::RemoteState::Unavailable);
            return;
        };
        let url = url.to_owned();
        let fetch_url = url.clone();
        let task =
            crate::bridge::tokio::spawn_net(cx, async move { client.fetch(&fetch_url).await });
        cx.spawn(async move |this, cx| {
            let state = match task.await {
                Ok(Ok(fetched)) => {
                    let bytes = fetched.bytes;
                    cx.background_executor()
                        .spawn(async move { remote::decode(bytes) })
                        .await
                }
                Ok(Err(err)) => {
                    tracing::debug!(%err, "preview: remote image refused or failed");
                    remote::RemoteState::Unavailable
                }
                Err(err) => {
                    tracing::debug!(%err, "preview: remote image task failed");
                    remote::RemoteState::Unavailable
                }
            };
            this.update(cx, |this, cx| {
                this.remote.finish(&url, state);
                this.list.remeasure();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keyboard on the pane (not inside the viewer).
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let page = self.list.viewport_bounds().size.height * 0.9;
        let line = Metrics::at(self.zoom).line_height * 3.0;
        let plain = !ks.modifiers.control && !ks.modifiers.alt && !ks.modifiers.platform;
        let handled = match ks.key.as_str() {
            "down" if plain => {
                self.list.scroll_by(line);
                true
            }
            "up" if plain => {
                self.list.scroll_by(-line);
                true
            }
            "pagedown" => {
                self.list.scroll_by(page);
                true
            }
            "pageup" => {
                self.list.scroll_by(-page);
                true
            }
            "home" if ks.modifiers.control || plain => {
                self.list.scroll_to(ListOffset::default());
                true
            }
            "end" if ks.modifiers.control || plain => {
                self.list.scroll_to_end();
                true
            }
            _ => false,
        };
        if handled {
            let _ = window;
            self.report_visible_line(cx);
            cx.stop_propagation();
            cx.notify();
        }
    }
}

use style::Metrics;
