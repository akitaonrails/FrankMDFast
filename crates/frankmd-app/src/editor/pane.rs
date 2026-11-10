//! [`EditorPane`]: the markdown editor view (gpui-kit `Editor` + tree-sitter markdown),
//! with the vim layer, typewriter mode, the inline find bar, the text format menu and
//! the formatting commands.
//!
//! # Key routing
//!
//! GPUI resolves key *bindings* before any `on_key_down` listener runs, and gpui-kit's
//! `Input` key context (the deepest one) binds `ctrl-f`, `ctrl-h`, `ctrl-enter`,
//! `escape`, arrows... So a capture-phase key listener would run too late for vim and
//! would lose `Ctrl+F`/`Ctrl+H`/`Ctrl+Enter` to gpui-kit's own search panel. The pane
//! therefore registers one **keystroke interceptor** (`App::intercept_keystrokes`, which
//! runs before bindings) and routes, while one of its parts has focus:
//! 1. the text format menu (arrows, Enter, Esc, mnemonics);
//! 2. the find bar inputs (Enter / Shift+Enter / Alt+Enter / Ctrl+Alt+Enter, Alt+C/R/W,
//!    Esc, F3);
//! 3. the editor: vim first (when on), then the editor-owned commands of the registry
//!    ([`OWNED_COMMANDS`]), then undo/redo (to report the created-note boundary).
//!
//! Nothing is intercepted while an IME composition is active (ui-spec §8.3).
//! Handled keys stop propagation; everything else reaches the normal keymap.

use crate::scale::px;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use frankmd_core::DocId;
use frankmd_markdown::line_numbers::LineNumberMode;
use frankmd_vim::buffer::ScrollAnchor;
use frankmd_vim::{ExCall, Mode, Outcome, VimState};
use frankmd_workspace::commands::CommandRegistry;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    BufferPoint, Editor, EditorState, Input, InputEvent, InputState, RangeDecoration,
    RangeDecorationCollection, RangeDecorationStyle, Redo, RopeExt as _, TabSize, Undo,
};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::edit::{Edit, minimal_replacement};
use super::find::{Counter, FindState};
use super::format::{self, FORMATS};
use super::settings::{EditorSettings, LINE_HEIGHT};
use super::vim::{EditorBuffer, scroll_to_line, vim_key, visible_lines};
use crate::ui::i18n;

/// The shell's per-command action callback (`crate::ui::actions::on_commands`).
type CommandHandler = Rc<dyn Fn(&'static str, &mut Window, &mut App)>;

/// Commands the editor pane runs itself (registry ids); every other command is the shell's.
pub const OWNED_COMMANDS: &[&str] = &[
    "format.bold",
    "format.italic",
    "format.strikethrough",
    "format.inline_code",
    "insert.link",
    "format.menu",
    "edit.toggle_task",
    "find.find",
    "find.replace",
    "find.next",
    "find.prev",
    "insert.table",
    "insert.code_block",
    "insert.emoji",
    "edit.context_action",
    "table.edit",
];

/// Debounce for recomputing find matches after a document edit.
const FIND_REFRESH_DEBOUNCE: Duration = Duration::from_millis(120);

/// What the pane tells the shell.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorEvent {
    /// The user (or an editor command) changed the text of `doc`. Read it with
    /// [`EditorPane::text`]. Not emitted for [`EditorPane::set_document`].
    Edited {
        /// The open document.
        doc: Option<DocId>,
    },
    /// Caret or selection moved (status line position, typewriter preview sync).
    SelectionChanged {
        /// Selection in UTF-8 bytes.
        selection: Range<usize>,
        /// 1-based line of the caret.
        line: usize,
        /// 1-based column of the caret (UTF-16 units, like FrankMD's status bar).
        column: usize,
    },
    /// The viewport scrolled; `top_line` is the 0-based first visible line (sync scroll).
    Scrolled {
        /// First visible buffer line.
        top_line: usize,
    },
    /// Vim mode, command line or message changed (status line mode block / message slot).
    VimStatusChanged,
    /// A vim ex command for the app (`:w`, `:q`, `:e`, ...).
    VimCommand(ExCall),
    /// Undo with nothing left to undo (FrankMD's created-note boundary, FE-062).
    UndoBoundary,
    /// Redo with nothing left to redo (Ctrl+Y / Ctrl+Shift+Z, FE-049).
    RedoBoundary,
    /// The find bar closed (focus is back in the editor).
    FindClosed,
}

/// The text format menu (Ctrl+M) while open.
#[derive(Debug, Clone)]
struct FormatMenu {
    selected: usize,
    selection: Range<usize>,
}

/// One registry binding the pane handles itself.
struct OwnedBinding {
    keystroke: Keystroke,
    command: &'static str,
}

/// The editor pane view.
pub struct EditorPane {
    pub(super) state: Entity<EditorState>,
    /// Observation-only handle on the `editor` wrapper (its scope contains the text focus).
    observed_focus: FocusHandle,
    window: AnyWindowHandle,
    doc: Option<DocId>,
    pub(super) settings: EditorSettings,
    pub(super) read_only: bool,
    pub(super) vim: VimState,
    block_cursor: RangeDecorationCollection,
    block_range: Option<Range<usize>>,
    find: FindState,
    find_input: Entity<InputState>,
    replace_input: Entity<InputState>,
    find_focus: FocusHandle,
    replace_focus: FocusHandle,
    find_all_marks: RangeDecorationCollection,
    find_current_mark: RangeDecorationCollection,
    find_refresh: Option<Task<()>>,
    feedback_timer: Option<Task<()>>,
    menu: Option<FormatMenu>,
    menu_focus: FocusHandle,
    bindings: Vec<OwnedBinding>,
    last_selection: Range<usize>,
    last_top_line: usize,
    last_scroll_y: Pixels,
    /// The next viewport move came from [`Self::sync_to_line`]; do not echo it.
    sync_scrolling: bool,
    /// Slash menu, completions, insert dialogs and the pending `/query` ([`super::insert`]).
    pub(super) insert: super::insert::InsertHost,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<EditorEvent> for EditorPane {}

impl Focusable for EditorPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle(cx)
    }
}

impl EditorPane {
    /// Create the pane (empty document).
    pub fn new(window: &mut Window, cx: &mut Context<Self>, settings: EditorSettings) -> Self {
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("markdown")
                .line_number(settings.line_numbers != LineNumberMode::Off)
                .soft_wrap(settings.soft_wrap)
                .searchable(false)
                .tab_size(tab_size(settings.indent))
        });
        let (block_cursor, find_all_marks, find_current_mark) = state.update(cx, |s, cx| {
            (
                s.create_range_decorations_collection(Vec::new(), cx),
                s.create_range_decorations_collection(Vec::new(), cx),
                s.create_range_decorations_collection(Vec::new(), cx),
            )
        });
        let find_placeholder = i18n::t(cx, "dialogs.find_replace.find_placeholder");
        let replace_placeholder = i18n::t(cx, "dialogs.find_replace.replace_placeholder");
        let find_input = cx.new(|cx| InputState::new(window, cx).placeholder(find_placeholder));
        let replace_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(replace_placeholder));

        let weak = cx.entity().downgrade();
        let interceptor = cx.intercept_keystrokes(move |event, window, cx| {
            let Some(pane) = weak.upgrade() else { return };
            let handled = pane.update(cx, |pane, cx| {
                pane.on_keystroke(&event.keystroke, window, cx)
            });
            if handled {
                cx.stop_propagation();
            }
        });
        let subscriptions = vec![
            interceptor,
            cx.subscribe_in(&state, window, Self::on_input_event),
            cx.observe_in(&state, window, |pane, _, window, cx| {
                pane.on_state_changed(window, cx)
            }),
            cx.subscribe_in(&find_input, window, Self::on_find_input_event),
            cx.subscribe_in(&replace_input, window, Self::on_replace_input_event),
        ];
        let vim = VimState::new(settings.macos);
        Self {
            state,
            observed_focus: cx.focus_handle(),
            window: window.window_handle(),
            doc: None,
            settings,
            read_only: false,
            vim,
            block_cursor,
            block_range: None,
            find: FindState::default(),
            find_input,
            replace_input,
            find_focus: cx.focus_handle(),
            replace_focus: cx.focus_handle(),
            find_all_marks,
            find_current_mark,
            find_refresh: None,
            feedback_timer: None,
            menu: None,
            menu_focus: cx.focus_handle(),
            bindings: owned_bindings(),
            last_selection: 0..0,
            last_top_line: 0,
            last_scroll_y: px(0.),
            sync_scrolling: false,
            insert: super::insert::InsertHost::default(),
            _subscriptions: subscriptions,
        }
    }

    // ----- document ----------------------------------------------------------

    /// Show `text` as document `doc` (opening a note, reload, recovery). Clears the undo
    /// history; not reported as [`EditorEvent::Edited`].
    pub fn set_document(
        &mut self,
        doc: Option<DocId>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.doc = doc;
        self.set_text(text, window, cx);
    }

    /// Replace the text, keeping the document id (see [`Self::set_document`]).
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_owned();
        self.state.update(cx, |s, cx| s.set_value(text, window, cx));
        self.menu = None;
        self.close_insert_surfaces(cx);
        self.vim = VimState::new(self.settings.macos);
        self.last_selection = 0..0;
        if self.find.open {
            self.refresh_find(cx);
        }
        self.update_block_cursor(cx);
        cx.emit(EditorEvent::VimStatusChanged);
        cx.notify();
    }

    /// The open document id.
    pub fn doc(&self) -> Option<DocId> {
        self.doc
    }

    /// The full text (copies the rope).
    pub fn text(&self, cx: &App) -> String {
        self.state.read(cx).text().to_string()
    }

    /// The text rope (cheap clone; for large-document consumers).
    pub fn rope(&self, cx: &App) -> gpui_kit::component::Rope {
        self.state.read(cx).text().clone()
    }

    /// The underlying gpui-kit editor state (for the shell's paste/drop hooks).
    pub fn editor_state(&self) -> &Entity<EditorState> {
        &self.state
    }

    /// Read-only (vault unreachable banner, FE read-only states).
    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        self.read_only = read_only;
        cx.notify();
    }

    /// Selection in UTF-8 bytes.
    pub fn selection(&self, cx: &App) -> Range<usize> {
        self.state.read(cx).selected_range()
    }

    /// Select `range` (UTF-8 bytes) and scroll it into view.
    pub fn set_selection(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.state
            .update(cx, |s, cx| s.set_selected_range(range, cx));
    }

    /// `(line, column)` of the caret, 1-based (column in UTF-16 units).
    pub fn caret(&self, cx: &App) -> (usize, usize) {
        let p = self.state.read(cx).cursor_position();
        (p.line as usize + 1, p.character as usize + 1)
    }

    /// Insert `text` over the selection as one undo step (dialogs: image, table, emoji...).
    pub fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selection(cx);
        let caret = selection.start + text.len();
        self.apply_edit(
            Edit {
                range: selection,
                text: text.to_owned(),
                selection: caret..caret,
            },
            window,
            cx,
        );
    }

    /// Apply `edit` as one undo step (minimized to the bytes that change).
    pub fn apply_edit(&mut self, edit: Edit, window: &mut Window, cx: &mut Context<Self>) {
        let old = self
            .state
            .read(cx)
            .text()
            .slice(edit.range.clone())
            .to_string();
        let (range, text) = minimal_replacement(&old, edit.range.start, &edit.text);
        let text = text.to_owned();
        let selection = edit.selection;
        if self.find.open && range.len() > 4096 {
            // gpui-kit re-anchors every decoration through an edit; with tens of
            // thousands of match marks a large replace costs seconds (measured: 2.7 s
            // vs 0.19 s on 5 MB). Drop them; the find refresh repaints them.
            self.find_all_marks.set(Vec::new(), cx);
            self.find_current_mark.set(Vec::new(), cx);
        }
        self.state.update(cx, |s, cx| {
            if !range.is_empty() || !text.is_empty() {
                s.set_selected_range(range, cx);
                s.replace(text, window, cx);
            }
            s.set_selected_range(selection, cx);
        });
    }

    // ----- focus, scrolling ---------------------------------------------------

    /// Focus the text.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Whether the text area (not the find bar or menu) has focus.
    pub fn is_text_focused(&self, window: &Window, cx: &App) -> bool {
        self.state.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Scroll so 0-based buffer `line` is the first visible line.
    pub fn scroll_to_line(&mut self, line: usize, cx: &mut Context<Self>) {
        scroll_to_line(&self.state, line, ScrollAnchor::Top, cx);
    }

    /// Like [`Self::scroll_to_line`], but does not echo an [`EditorEvent::Scrolled`]
    /// (sync scroll from the preview; an echo would yank the preview back).
    pub fn sync_to_line(&mut self, line: usize, cx: &mut Context<Self>) {
        // shortcut: if the viewport does not move, the flag swallows the next user
        // scroll's echo once; track the target offset if that ever shows.
        self.sync_scrolling = true;
        self.scroll_to_line(line, cx);
    }

    /// 0-based first visible buffer line.
    pub fn top_visible_line(&self, cx: &App) -> usize {
        visible_lines(&self.state, cx).start
    }

    // ----- settings ---------------------------------------------------------------

    /// Current settings.
    pub fn settings(&self) -> &EditorSettings {
        &self.settings
    }

    /// Apply font, size, vim, typewriter, line numbers, wrap, width and indent.
    pub fn apply_settings(
        &mut self,
        settings: EditorSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old = std::mem::replace(&mut self.settings, settings);
        let new = self.settings.clone();
        self.state.update(cx, |s, cx| {
            s.set_line_number(new.line_numbers != LineNumberMode::Off, window, cx);
            s.set_soft_wrap(new.soft_wrap, window, cx);
            s.set_tab_size(tab_size(new.indent), cx);
        });
        if old.vim != new.vim || old.macos != new.macos {
            self.vim = VimState::new(new.macos);
            self.update_block_cursor(cx);
            cx.emit(EditorEvent::VimStatusChanged);
        }
        if old.typewriter != new.typewriter {
            self.update_typewriter(window, cx);
            if new.typewriter {
                let line = self.caret(cx).0 - 1;
                scroll_to_line(&self.state, line, ScrollAnchor::Center, cx);
            }
        }
        cx.notify();
    }

    // ----- vim status -----------------------------------------------------------------

    /// The vim mode (`None` when vim is off). The status line shows `NORMAL`, `INSERT`...
    pub fn vim_mode(&self) -> Option<Mode> {
        self.settings.vim.then(|| self.vim.mode())
    }

    /// The status line's mode block text (ui-spec §3.6): `NORMAL`, `INSERT`, `VISUAL`,
    /// `V-LINE`, `V-BLOCK`, `REPLACE` or `COMMAND`; `None` when vim is off.
    pub fn vim_mode_label(&self) -> Option<&'static str> {
        self.vim_mode().map(|mode| match mode {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
            Mode::VisualBlock => "V-BLOCK",
            Mode::Replace => "REPLACE",
            Mode::CommandLine => "COMMAND",
        })
    }

    /// The `:`/`/` line being typed.
    pub fn vim_command_line(&self) -> Option<String> {
        if !self.settings.vim {
            return None;
        }
        self.vim.command_line().map(str::to_owned)
    }

    /// The last vim message (`E348: No string under cursor`...).
    pub fn vim_message(&self) -> Option<String> {
        if !self.settings.vim {
            return None;
        }
        self.vim.message().map(str::to_owned)
    }

    // ----- commands -----------------------------------------------------------------------

    /// Whether the pane runs registry command `id` itself.
    pub fn owns_command(id: &str) -> bool {
        OWNED_COMMANDS.contains(&id)
    }

    /// Run an editor-owned command (palette rows, actions). `false` if `id` isn't ours.
    pub fn run_command(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match id {
            "format.bold" => self.wrap_with("**", "**", window, cx),
            "format.italic" => self.wrap_with("*", "*", window, cx),
            "format.strikethrough" => self.wrap_with("~~", "~~", window, cx),
            "format.inline_code" => self.wrap_with("`", "`", window, cx),
            "insert.link" => {
                let (text, sel) = self.text_and_selection(cx);
                self.apply_edit(format::insert_link(&text, sel), window, cx);
                self.open_link_completion(cx);
            }
            "insert.table" | "table.edit" => self.open_table_editor(window, cx),
            "insert.code_block" => self.open_code_dialog(window, cx),
            "insert.emoji" => self.open_emoji_picker(window, cx),
            // Nothing to act on at the caret: unhandled, so the shell reports it.
            "edit.context_action" => return self.run_context_action(window, cx).is_some(),
            "format.menu" => self.open_format_menu(window, cx),
            "edit.toggle_task" => {
                let (text, sel) = self.text_and_selection(cx);
                if let Some(edit) = format::toggle_tasks(&text, sel) {
                    self.apply_edit(edit, window, cx);
                }
            }
            "find.find" => self.open_find(false, window, cx),
            "find.replace" => self.open_find(true, window, cx),
            "find.next" => self.find_step(true, window, cx),
            "find.prev" => self.find_step(false, window, cx),
            _ => return false,
        }
        true
    }

    fn text_and_selection(&self, cx: &App) -> (String, Range<usize>) {
        let s = self.state.read(cx);
        (s.text().to_string(), s.selected_range())
    }

    fn wrap_with(
        &mut self,
        prefix: &str,
        suffix: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, sel) = self.text_and_selection(cx);
        self.apply_edit(
            format::wrap_selection(&text, sel, prefix, suffix),
            window,
            cx,
        );
    }

    // ----- key routing ------------------------------------------------------------------------

    fn on_keystroke(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if window.window_handle() != self.window {
            return false;
        }
        if self.menu.is_some() && self.menu_focus.is_focused(window) {
            return self.menu_key(keystroke, window, cx);
        }
        if self.find.open {
            let in_find = self.find_input.read(cx).focus_handle(cx).is_focused(window);
            let in_replace = self
                .replace_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window);
            if in_find || in_replace {
                let input = if in_find {
                    self.find_input.clone()
                } else {
                    self.replace_input.clone()
                };
                if input
                    .update(cx, |s, cx| s.marked_text_range(window, cx))
                    .is_some()
                {
                    return false;
                }
                return self.find_bar_key(keystroke, window, cx);
            }
        }
        if !self.is_text_focused(window, cx) {
            return false;
        }
        if self
            .state
            .update(cx, |s, cx| s.marked_text_range(window, cx))
            .is_some()
        {
            return false;
        }
        if let Some(handled) = self.popover_key(&keystroke_text(keystroke), window, cx) {
            return handled;
        }
        if self.settings.vim
            && let Some(handled) = self.vim_key(keystroke, window, cx)
        {
            return handled;
        }
        if let Some(command) = self
            .bindings
            .iter()
            .find(|b| same_keystroke(&b.keystroke, keystroke))
            .map(|b| b.command)
        {
            return self.run_command(command, window, cx);
        }
        match keystroke_text(keystroke).as_str() {
            "ctrl-z" if !self.read_only => self.history(true, window, cx),
            "ctrl-y" | "ctrl-shift-z" if !self.read_only => self.history(false, window, cx),
            "escape" if self.find.open => {
                self.close_find(window, cx);
                true
            }
            _ => false,
        }
    }

    /// Feed vim. `None` = vim passed the key on (try the editor commands next).
    fn vim_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        let key = vim_key(keystroke)?;
        let mode_before = self.vim.mode();
        let outcome = {
            let mut buf = EditorBuffer::new(&self.state, window, cx);
            let outcome = self.vim.feed(&key, false, &mut buf);
            buf.flush();
            outcome
        };
        self.vim.take_mode_change();
        self.update_block_cursor(cx);
        cx.emit(EditorEvent::VimStatusChanged);
        match outcome {
            // Esc in normal mode also reaches the app's Escape chain (close the find
            // bar, overlays...), as it did with codemirror-vim.
            Outcome::Handled if key.key == "escape" && mode_before == Mode::Normal => {
                if self.find.open {
                    self.close_find(window, cx);
                    return Some(true);
                }
                Some(false)
            }
            Outcome::Handled => Some(true),
            Outcome::App(call) => {
                cx.emit(EditorEvent::VimCommand(call));
                Some(true)
            }
            Outcome::PassThrough => {
                let modal = matches!(
                    self.vim.mode(),
                    Mode::Normal | Mode::Visual | Mode::VisualLine | Mode::VisualBlock
                );
                let plain =
                    !key.ctrl && !key.alt && !is_function_key(&key.key) && key.key != "escape";
                // Unbound printable keys must not type into the text in normal mode.
                if modal && plain { Some(true) } else { None }
            }
        }
    }

    /// Undo/redo, reporting the history boundary when nothing changed.
    fn history(&mut self, undo: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let before = self.state.read(cx).text().clone();
        let focus = self.state.read(cx).focus_handle(cx);
        if undo {
            focus.dispatch_action(&Undo, window, cx);
        } else {
            focus.dispatch_action(&Redo, window, cx);
        }
        let after = self.state.read(cx).text();
        if after.len() == before.len() && *after == before {
            cx.emit(if undo {
                EditorEvent::UndoBoundary
            } else {
                EditorEvent::RedoBoundary
            });
        }
        true
    }

    // ----- editor state observation ---------------------------------------------------------------

    fn on_input_event(
        &mut self,
        _: &Entity<EditorState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            cx.emit(EditorEvent::Edited { doc: self.doc });
            self.refresh_popovers_after_edit(cx);
            if self.find.open {
                self.schedule_find_refresh(cx);
            }
        }
    }

    fn on_state_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (selection, scroll_y) = {
            let s = self.state.read(cx);
            (s.selected_range(), s.scroll_offset().y)
        };
        if selection != self.last_selection {
            self.last_selection = selection.clone();
            let (line, column) = self.caret(cx);
            let empty = selection.is_empty();
            cx.emit(EditorEvent::SelectionChanged {
                selection,
                line,
                column,
            });
            self.update_block_cursor(cx);
            self.refresh_popovers_after_move(cx);
            if self.settings.typewriter && empty {
                center_caret(&self.state, cx);
            }
        }
        if scroll_y != self.last_scroll_y {
            self.last_scroll_y = scroll_y;
            let top_line = self.top_visible_line(cx);
            if std::mem::take(&mut self.sync_scrolling) {
                self.last_top_line = top_line;
            } else if top_line != self.last_top_line {
                self.last_top_line = top_line;
                cx.emit(EditorEvent::Scrolled { top_line });
            }
        }
        self.update_typewriter(window, cx);
    }

    /// Typewriter mode (FE A.10): pad the bottom by half the viewport and keep the caret
    /// row centred (`cursor_surrounding_lines` = half the rows) unless a selection exists.
    fn update_typewriter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (rows, has_selection) = {
            let s = self.state.read(cx);
            let height = s.input_bounds().size.height;
            let line_height = s.line_height().unwrap_or(px(20.));
            let rows = if line_height > px(0.) {
                (height / line_height).floor() as usize
            } else {
                0
            };
            (rows, !s.selected_range().is_empty())
        };
        let wanted = self.settings.typewriter.then_some(rows / 2);
        let surrounding = if has_selection { None } else { wanted };
        // Both setters are no-ops (no notify) when the value is unchanged.
        self.state.update(cx, |s, cx| {
            s.set_scroll_beyond_last_line(wanted, window, cx);
            s.set_cursor_surrounding_lines(surrounding, window, cx);
        });
    }

    /// Vim's block cursor: a filled box over the character under the caret in normal mode
    /// (**[COMPAT]** FrankMD's themed fat cursor). The beam caret still blinks at its
    /// left edge: gpui-kit has no caret-shape API and re-projects the caret colour from
    /// the global theme on every render.
    fn update_block_cursor(&mut self, cx: &mut Context<Self>) {
        let range = if self.settings.vim && self.vim.mode() == Mode::Normal {
            let s = self.state.read(cx);
            let sel = s.selected_range();
            match s.text().char_at(sel.end) {
                Some(c) if sel.is_empty() && c != '\n' => Some(sel.end..sel.end + c.len_utf8()),
                _ => None,
            }
        } else {
            None
        };
        if range == self.block_range {
            return;
        }
        self.block_range = range.clone();
        let color = cx.theme().caret.opacity(0.45);
        let decorations = range
            .map(|r| {
                vec![
                    RangeDecoration::new(r)
                        .with_style(RangeDecorationStyle::Fill)
                        .with_color(color),
                ]
            })
            .unwrap_or_default();
        self.block_cursor.set(decorations, cx);
    }

    // ----- find bar -----------------------------------------------------------------------------------

    /// Find state (counter, matches) for the shell's status/palette.
    pub fn find_state(&self) -> &FindState {
        &self.find
    }

    /// Open the find bar (Ctrl+F) or find + replace (Ctrl+H), pre-filled with the selection.
    pub fn open_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (text, sel) = self.text_and_selection(cx);
        let prefill = text
            .get(sel.clone())
            .filter(|s| !s.is_empty() && !s.contains('\n'))
            .map(str::to_owned);
        self.find.open = true;
        self.find.replace_visible = replace || (self.find.replace_visible && self.find.open);
        if replace {
            self.find.replace_visible = true;
        }
        if let Some(query) = prefill {
            self.find.query.clone_from(&query);
            self.find_input
                .update(cx, |s, cx| s.set_value(query, window, cx));
        }
        self.find_input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.find.recompute(&text, sel.start);
        self.paint_matches(cx);
        cx.notify();
    }

    /// Close the bar; the editor gets focus with the current match selected.
    pub fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(range) = self.find.current_range() {
            self.set_selection(range, cx);
        }
        self.find.open = false;
        self.find.clear_matches();
        self.find_refresh = None;
        self.paint_matches(cx);
        self.focus(window, cx);
        cx.emit(EditorEvent::FindClosed);
        cx.notify();
    }

    /// Whether the find bar is open.
    pub fn is_find_open(&self) -> bool {
        self.find.open
    }

    fn find_step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (text, sel) = self.text_and_selection(cx);
        if self.find.query.is_empty() {
            if !self.find.open {
                self.open_find(false, window, cx);
            }
            return;
        }
        if let Some(range) = self.find.step(&text, sel, forward) {
            self.set_selection(range, cx);
        }
        self.paint_matches(cx);
        cx.notify();
    }

    fn replace_current(&mut self, advance: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.find.replace_current() else {
            return;
        };
        let after = edit.selection.end;
        self.apply_edit(edit, window, cx);
        let text = self.text(cx);
        self.find.recompute(&text, after);
        if advance && let Some(range) = self.find.step(&text, after..after, true) {
            self.set_selection(range, cx);
        }
        self.paint_matches(cx);
        cx.notify();
    }

    fn replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, sel) = self.text_and_selection(cx);
        let Some((edit, _count)) = self.find.replace_all(&text, sel.start, Instant::now()) else {
            return;
        };
        self.apply_edit(edit, window, cx);
        let text = self.text(cx);
        let caret = self.selection(cx).start;
        self.find.recompute(&text, caret);
        self.paint_matches(cx);
        self.feedback_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(super::find::FEEDBACK_TTL)
                .await;
            this.update(cx, |_, cx| cx.notify()).ok();
        }));
        cx.notify();
    }

    fn schedule_find_refresh(&mut self, cx: &mut Context<Self>) {
        self.find_refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIND_REFRESH_DEBOUNCE).await;
            this.update(cx, |pane, cx| pane.refresh_find(cx)).ok();
        }));
    }

    /// Recompute matches against the current text (live on document edits).
    fn refresh_find(&mut self, cx: &mut Context<Self>) {
        let (text, sel) = self.text_and_selection(cx);
        self.find.recompute(&text, sel.start);
        self.paint_matches(cx);
        cx.notify();
    }

    /// All matches filled with the selection colour at 50 %, the current one framed in accent.
    fn paint_matches(&mut self, cx: &mut Context<Self>) {
        let fill = cx.theme().selection.opacity(0.5);
        let accent = cx.theme().accent;
        let all = if self.find.open {
            self.find
                .ranges()
                .iter()
                .map(|r| {
                    RangeDecoration::new(r.clone())
                        .with_style(RangeDecorationStyle::Fill)
                        .with_color(fill)
                })
                .collect()
        } else {
            Vec::new()
        };
        let current = self
            .find
            .current_range()
            .filter(|_| self.find.open)
            .map(|r| {
                vec![
                    RangeDecoration::new(r)
                        .with_style(RangeDecorationStyle::Frame)
                        .with_color(accent),
                ]
            })
            .unwrap_or_default();
        self.find_all_marks.set(all, cx);
        self.find_current_mark.set(current, cx);
    }

    fn on_find_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.find.query = input.read(cx).value().to_string();
            self.refresh_find(cx);
        }
    }

    fn on_replace_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.find.replacement = input.read(cx).value().to_string();
        }
    }

    fn toggle_option(&mut self, which: char, cx: &mut Context<Self>) {
        let o = &mut self.find.options;
        match which {
            'c' => o.case_sensitive = !o.case_sensitive,
            'r' => o.use_regex = !o.use_regex,
            _ => o.whole_word = !o.whole_word,
        }
        self.refresh_find(cx);
    }

    fn find_bar_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match keystroke_text(keystroke).as_str() {
            "enter" | "f3" => self.find_step(true, window, cx),
            "shift-enter" | "shift-f3" => self.find_step(false, window, cx),
            "alt-enter" if self.find.replace_visible => self.replace_current(true, window, cx),
            "ctrl-alt-enter" if self.find.replace_visible => self.replace_all(window, cx),
            "alt-c" => self.toggle_option('c', cx),
            "alt-r" => self.toggle_option('r', cx),
            "alt-w" => self.toggle_option('w', cx),
            "escape" => self.close_find(window, cx),
            "ctrl-f" => self.open_find(false, window, cx),
            "ctrl-h" => self.open_find(true, window, cx),
            _ => return false,
        }
        true
    }

    fn counter_text(&self, cx: &App) -> (SharedString, Option<Hsla>) {
        match self.find.counter(Instant::now()) {
            Counter::Empty => ("".into(), None),
            Counter::NoResults => (i18n::t(cx, "dialogs.find_replace.no_results"), None),
            Counter::Matches { current, total } => {
                let (current, total) = (current.to_string(), total.to_string());
                (
                    i18n::t_args(
                        cx,
                        "dialogs.find_replace.match_count",
                        &[("current", &current), ("total", &total)],
                    ),
                    None,
                )
            }
            Counter::Error(error) => (error.into(), Some(cx.theme().danger)),
            Counter::Replaced(count) => {
                let count = count.to_string();
                (
                    i18n::t_args(
                        cx,
                        "dialogs.find_replace.replaced_count",
                        &[("count", &count)],
                    ),
                    Some(cx.theme().success),
                )
            }
        }
    }

    // ----- text format menu ---------------------------------------------------------------------------

    /// Ctrl+M: open the format menu at the caret (only for a non-blank selection).
    pub fn open_format_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, sel) = self.text_and_selection(cx);
        if text.get(sel.clone()).is_none_or(|s| s.trim().is_empty()) {
            return;
        }
        self.menu = Some(FormatMenu {
            selected: 0,
            selection: sel,
        });
        self.menu_focus.focus(window, cx);
        cx.notify();
    }

    /// Whether the format menu is open.
    pub fn is_format_menu_open(&self) -> bool {
        self.menu.is_some()
    }

    fn close_format_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.focus(window, cx);
        cx.notify();
    }

    fn apply_format_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else { return };
        let Some(format) = FORMATS.get(index) else {
            return;
        };
        let text = self.text(cx);
        let edit = format::apply_menu_format(&text, menu.selection, format);
        self.apply_edit(edit, window, cx);
        self.focus(window, cx);
        cx.notify();
    }

    fn menu_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(menu) = self.menu.as_mut() else {
            return false;
        };
        let n = FORMATS.len();
        match keystroke_text(keystroke).as_str() {
            "down" => menu.selected = (menu.selected + 1) % n,
            "up" => menu.selected = (menu.selected + n - 1) % n,
            "enter" => {
                let index = menu.selected;
                self.apply_format_row(index, window, cx);
            }
            "escape" => self.close_format_menu(window, cx),
            other => {
                let mnemonic = other.strip_prefix("shift-").unwrap_or(other);
                let mut chars = mnemonic.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => match format::format_by_hotkey(c) {
                        Some(f) => {
                            let index = FORMATS.iter().position(|x| x.id == f.id).unwrap_or(0);
                            self.apply_format_row(index, window, cx);
                        }
                        None => return true,
                    },
                    _ => return false,
                }
            }
        }
        cx.notify();
        true
    }

    // ----- rendering ------------------------------------------------------------------------------------

    fn column_width(&self, window: &Window, family: &SharedString, size: Pixels) -> Pixels {
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font(family.clone()));
        let ch = text_system
            .ch_advance(font_id, size)
            .ok()
            .filter(|w| *w > px(0.))
            .unwrap_or(size * 0.6);
        let gutter = if self.settings.line_numbers == LineNumberMode::Off {
            px(0.)
        } else {
            ch * 7.
        };
        // Text column plus the 32 px inline padding on both sides (ui-spec §3.1).
        ch * f32::from(self.settings.width_ch) + gutter + px(64.)
    }

    fn render_find_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let (counter, counter_color) = self.counter_text(cx);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let opts = self.find.options;
        let toggle =
            |id: &'static str, label: &'static str, on: bool, which: char, tip: SharedString| {
                Button::new(id)
                    .label(label)
                    .xsmall()
                    .ghost()
                    .toggled(on)
                    .tooltip(tip)
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_option(which, cx)))
            };
        let find_row = h_flex()
            .gap_1()
            .w_full()
            .child(
                div()
                    .id("find-input")
                    .test_support()
                    .track_focus(&self.find_focus)
                    .flex_1()
                    .child(Input::new(&self.find_input).xsmall()),
            )
            .child(
                div()
                    .min_w(px(72.))
                    .text_xs()
                    .text_color(counter_color.unwrap_or(muted))
                    .child(counter),
            )
            .child(
                Button::new("find-prev")
                    .icon(crate::ui::icons::ph("arrow-up"))
                    .xsmall()
                    .ghost()
                    .tooltip(i18n::t(cx, "dialogs.find_replace.previous"))
                    .on_click(cx.listener(|this, _, window, cx| this.find_step(false, window, cx))),
            )
            .child(
                Button::new("find-next")
                    .icon(crate::ui::icons::ph("arrow-down"))
                    .xsmall()
                    .ghost()
                    .tooltip(i18n::t(cx, "dialogs.find_replace.next"))
                    .on_click(cx.listener(|this, _, window, cx| this.find_step(true, window, cx))),
            )
            .child(toggle(
                "find-case",
                "Aa",
                opts.case_sensitive,
                'c',
                i18n::t(cx, "dialogs.find_replace.case_sensitive"),
            ))
            .child(toggle(
                "find-regex",
                ".*",
                opts.use_regex,
                'r',
                i18n::t(cx, "dialogs.find_replace.use_regex"),
            ))
            .child(toggle(
                "find-word",
                "W",
                opts.whole_word,
                'w',
                i18n::t(cx, "dialogs.find_replace.whole_word"),
            ))
            .child(
                Button::new("find-close")
                    .icon(crate::ui::icons::ph("x"))
                    .xsmall()
                    .ghost()
                    .tooltip(i18n::t(cx, "common.close"))
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            );
        let replace_row = h_flex()
            .gap_1()
            .w_full()
            .child(
                div()
                    .id("replace-input")
                    .test_support()
                    .track_focus(&self.replace_focus)
                    .flex_1()
                    .child(Input::new(&self.replace_input).xsmall()),
            )
            .child(
                Button::new("replace-one")
                    .label(i18n::t(cx, "dialogs.find_replace.replace"))
                    .xsmall()
                    .tooltip(i18n::t(cx, "dialogs.find_replace.replace_one_title"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.replace_current(true, window, cx)),
                    ),
            )
            .child(
                Button::new("replace-skip")
                    .label(i18n::t(cx, "dialogs.find_replace.skip"))
                    .xsmall()
                    .ghost()
                    .on_click(cx.listener(|this, _, window, cx| this.find_step(true, window, cx))),
            )
            .child(
                Button::new("replace-all")
                    .label(i18n::t(cx, "dialogs.find_replace.replace_all"))
                    .xsmall()
                    .tooltip(i18n::t(cx, "dialogs.find_replace.replace_all_title"))
                    .on_click(cx.listener(|this, _, window, cx| this.replace_all(window, cx))),
            );
        let theme = cx.theme();
        v_flex()
            .id("find-bar")
            .test_support()
            .w_full()
            .gap_1()
            .px_2()
            .py_1()
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border)
            .child(find_row)
            .when(self.find.replace_visible, |el| el.child(replace_row))
            .into_any_element()
    }

    fn render_format_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let state = self.state.read(cx);
        let anchor = state
            .range_to_bounds(&(menu.selection.end..menu.selection.end))
            .map(|b| b.bottom_left())
            .unwrap_or_else(|| state.input_bounds().center());
        let theme = cx.theme();
        let rows = FORMATS.iter().enumerate().map(|(i, f)| {
            let selected = i == menu.selected;
            let label = i18n::t(cx, &f.label_key());
            let (before, mnemonic, after) = split_mnemonic(&label, f.hotkey);
            div()
                .id(SharedString::from(format!("text-format-row-{}", f.id)))
                .test_support()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap_4()
                .px_3()
                .py(px(4.))
                .text_sm()
                .when(selected, |el| {
                    el.bg(theme.accent).text_color(theme.accent_foreground)
                })
                .when(!selected, |el| el.text_color(theme.foreground))
                .child(
                    h_flex()
                        .child(before)
                        .when_some(mnemonic, |el, m| el.child(div().underline().child(m)))
                        .child(after),
                )
                .child(div().text_xs().child(f.hotkey.to_string()))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered && let Some(menu) = this.menu.as_mut() {
                        menu.selected = i;
                        cx.notify();
                    }
                }))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.apply_format_row(i, window, cx)),
                )
        });
        let list = v_flex()
            .id("text-format-menu")
            .test_support()
            .track_focus(&self.menu_focus)
            .w(px(240.))
            .py_1()
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .children(rows.collect::<Vec<_>>());
        Some(
            deferred(
                anchored()
                    .position(anchor)
                    .snap_to_window_with_margin(px(8.))
                    .child(list),
            )
            .into_any_element(),
        )
    }
}

impl Render for EditorPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let family = self
            .settings
            .font_family
            .clone()
            .unwrap_or_else(|| cx.theme().mono_font_family.clone());
        let size = px(self.settings.font_size);
        let width = self.column_width(window, &family, size);
        let weak = cx.entity().downgrade();
        let handler: CommandHandler = Rc::new(move |id, window, cx| {
            let handled = EditorPane::owns_command(id)
                && weak
                    .update(cx, |pane, cx| pane.run_command(id, window, cx))
                    .unwrap_or(false);
            if !handled {
                cx.propagate();
            }
        });
        let root = crate::ui::actions::on_commands(v_flex(), handler);
        let find_bar = if self.find.open {
            Some(self.render_find_bar(cx))
        } else {
            None
        };
        let menu = self.render_format_menu(cx);
        let insert_layers = self.render_insert_layers(window, cx);
        root.key_context("Editor")
            .size_full()
            .children(find_bar)
            .child(
                div().flex_1().min_h_0().w_full().child(
                    div()
                        .id("editor")
                        .test_support()
                        .track_focus(&self.observed_focus)
                        .h_full()
                        .w_full()
                        .max_w(width)
                        .mx_auto()
                        .px(px(32.))
                        .py(px(16.))
                        .child(
                            Editor::new(&self.state)
                                .h(relative(1.))
                                .appearance(false)
                                .readonly(self.read_only)
                                .font_family(family)
                                .text_size(size)
                                .line_height(relative(LINE_HEIGHT)),
                        ),
                ),
            )
            .children(menu)
            .children(insert_layers)
    }
}

/// Typewriter: scroll so the caret's display row sits in the middle of the viewport.
/// Programmatic selection changes (jumps, find, vim) reveal with minimal padding in
/// gpui-kit, so the pane re-centres after every caret move like FrankMD's
/// `codemirror_typewriter.js` (keyboard motion is also kept centred by
/// `cursor_surrounding_lines`). Leaves the scroll alone if the row is already centred.
fn center_caret(state: &Entity<EditorState>, cx: &mut App) {
    state.update(cx, |s, cx| {
        let Some(line_height) = s.line_height() else {
            return;
        };
        let offset = s.selected_range().end;
        let text = s.text();
        let pos = text.offset_to_point(offset);
        let column = offset - text.line_start_offset(pos.row);
        let row = s
            .display_map()
            .buffer_pos_to_display_pos(BufferPoint::new(pos.row, column))
            .row;
        let viewport = s.input_bounds().size.height;
        let target = (line_height * row as f32 - (viewport - line_height) / 2.).max(px(0.));
        let current = -s.scroll_offset().y;
        if (target - current).abs() >= line_height {
            let x = s.scroll_offset().x;
            s.set_scroll_offset(point(x, -target), cx);
        }
    });
}

/// The registry's bindings for the commands the pane owns, parsed once.
fn owned_bindings() -> Vec<OwnedBinding> {
    CommandRegistry::builtin()
        .bindings()
        .into_iter()
        .filter(|b| OWNED_COMMANDS.contains(&b.command))
        .filter_map(|b| {
            Keystroke::parse(b.keystroke)
                .ok()
                .map(|keystroke| OwnedBinding {
                    keystroke,
                    command: b.command,
                })
        })
        .collect()
}

fn same_keystroke(a: &Keystroke, b: &Keystroke) -> bool {
    a.modifiers == b.modifiers && a.key.eq_ignore_ascii_case(&b.key)
}

/// `ctrl-alt-enter`-style text of a keystroke (modifiers in a fixed order).
fn keystroke_text(k: &Keystroke) -> String {
    let m = k.modifiers;
    let mut s = String::new();
    for (on, name) in [
        (m.control, "ctrl-"),
        (m.alt, "alt-"),
        (m.shift, "shift-"),
        (m.platform, "cmd-"),
    ] {
        if on {
            s.push_str(name);
        }
    }
    s.push_str(&k.key.to_ascii_lowercase());
    s
}

fn is_function_key(key: &str) -> bool {
    key.len() >= 2 && key.starts_with('f') && key[1..].bytes().all(|b| b.is_ascii_digit())
}

fn tab_size(indent: usize) -> TabSize {
    TabSize {
        tab_size: indent.max(1),
        hard_tabs: indent == 0,
    }
}

/// Split a label around its mnemonic letter (first case-insensitive occurrence).
fn split_mnemonic(label: &str, hotkey: char) -> (String, Option<String>, String) {
    let lower = hotkey.to_ascii_lowercase();
    match label
        .char_indices()
        .find(|(_, c)| c.to_ascii_lowercase() == lower)
    {
        Some((i, c)) => (
            label[..i].to_owned(),
            Some(c.to_string()),
            label[i + c.len_utf8()..].to_owned(),
        ),
        None => (label.to_owned(), None, String::new()),
    }
}
