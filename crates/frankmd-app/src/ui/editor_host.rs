//! The editor slot of the shell: owns the [`EditorPane`] entity and turns its
//! events into workspace commands (edits → `Command::Edited`, vim ex
//! commands → registry commands, caret → status line).

use frankmd_config::{Key, ResolvedConfig, Value};
use frankmd_markdown::line_numbers::{LineNumberMode, normalize_line_number_mode};
use frankmd_vim::ex::AppCommand;
use gpui_kit::*;

use super::shell::Shell;
use crate::editor::{EditorEvent, EditorPane, EditorSettings};

/// Editor slot.
pub(crate) struct EditorHost {
    pub(crate) pane: Entity<EditorPane>,
    caret: (usize, usize),
}

impl EditorHost {
    /// Create and subscribe the shell to the pane's events.
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Shell>) -> (Self, Subscription) {
        let pane = cx.new(|cx| EditorPane::new(window, cx, EditorSettings::default()));
        let sub = cx.subscribe_in(
            &pane,
            window,
            |shell, pane, ev: &EditorEvent, window, cx| match ev {
                EditorEvent::Edited { .. } => {
                    let text = pane.read(cx).text(cx);
                    shell.editor_changed(text, window, cx);
                }
                EditorEvent::SelectionChanged { line, column, .. } => {
                    shell.editor.caret = (*line, *column);
                    cx.notify();
                }
                EditorEvent::VimStatusChanged | EditorEvent::FindClosed => cx.notify(),
                EditorEvent::VimCommand(call) => {
                    let command = match call.command {
                        AppCommand::Save => "file.save",
                        AppCommand::CloseDialogs => "app.escape",
                        AppCommand::SaveAndClose => {
                            shell.run_command("file.save", window, cx);
                            "app.escape"
                        }
                        AppCommand::Help => "help.toggle",
                        AppCommand::OpenFinder => "file.quick_open",
                        AppCommand::ToggleSidebar => "view.toggle_sidebar",
                        AppCommand::NextNote | AppCommand::PrevNote => {
                            shell.step_note(call.command == AppCommand::NextNote, window, cx);
                            return;
                        }
                    };
                    shell.run_command(command, window, cx);
                }
                EditorEvent::Scrolled { top_line, within } => {
                    if shell.scroll_sync {
                        let line = u32::try_from(*top_line + 1).unwrap_or(u32::MAX);
                        shell
                            .preview
                            .update(cx, |p, cx| p.scroll_to_source_line(line, *within, cx));
                    }
                }
                EditorEvent::UndoBoundary | EditorEvent::RedoBoundary => {
                    let key = if matches!(ev, EditorEvent::UndoBoundary) {
                        "status.nothing_to_undo"
                    } else {
                        "status.nothing_to_redo"
                    };
                    let text = super::i18n::t(cx, key);
                    shell.show_message(&text, false, cx);
                }
            },
        );
        (
            Self {
                pane,
                caret: (1, 1),
            },
            sub,
        )
    }

    /// Replace the text (opening a note, reload, recovery); not reported as an edit.
    pub(crate) fn set_document(
        &mut self,
        doc: Option<frankmd_core::DocId>,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.caret = (1, 1);
        self.pane
            .update(cx, |pane, cx| pane.set_document(doc, text, window, cx));
    }

    /// Replace the text of the open document.
    pub(crate) fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut App) {
        self.pane
            .update(cx, |pane, cx| pane.set_text(text, window, cx));
    }

    /// Current text.
    pub(crate) fn text(&self, cx: &App) -> String {
        self.pane.read(cx).text(cx)
    }

    /// The focus handle of the text area.
    pub(crate) fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.pane.read(cx).focus_handle(cx)
    }

    /// Focus the text.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.pane.update(cx, |pane, cx| pane.focus(window, cx));
    }

    /// `(line, column)` of the caret, 1-based.
    pub(crate) fn caret(&self) -> (usize, usize) {
        self.caret
    }

    /// Vim mode label for the status line (`NORMAL`, `INSERT`, ...), when vim is on.
    pub(crate) fn vim_mode(&self, cx: &App) -> Option<&'static str> {
        self.pane.read(cx).vim_mode_label()
    }

    /// The vim ex command line / message for the status line message slot.
    pub(crate) fn vim_line(&self, cx: &App) -> Option<String> {
        let pane = self.pane.read(cx);
        pane.vim_command_line()
            .map(|l| format!(":{l}"))
            .or_else(|| pane.vim_message())
    }

    /// Run an editor-owned registry command. `false` when the pane doesn't own it.
    pub(crate) fn run_command(&self, id: &str, window: &mut Window, cx: &mut App) -> bool {
        EditorPane::owns_command(id)
            && self
                .pane
                .update(cx, |pane, cx| pane.run_command(id, window, cx))
    }

    /// Apply `.fed` editor settings; `font` is the fontconfig-resolved family.
    pub(crate) fn apply_config(
        &self,
        config: &ResolvedConfig,
        font: Option<SharedString>,
        read_only: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        let int = |key: Key, default: i64| {
            config
                .value(key)
                .and_then(|v| v.as_int())
                .unwrap_or(default)
        };
        let bool_of = |key: Key| config.value(key).and_then(|v| v.as_bool()).unwrap_or(false);
        let line_numbers = normalize_line_number_mode(
            config
                .value(Key::EditorLineNumbers)
                .map(|v| match v {
                    Value::Int(i) => i.to_string(),
                    Value::Str(s) => s,
                    other => other.to_json().to_string(),
                })
                .as_deref(),
            LineNumberMode::Off,
        );
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let settings = EditorSettings {
            font_family: font,
            font_size: int(Key::EditorFontSize, 14).clamp(8, 48) as f32,
            vim: bool_of(Key::VimMode),
            typewriter: bool_of(Key::TypewriterMode),
            line_numbers,
            soft_wrap: true,
            width_ch: int(Key::EditorWidth, 72).clamp(40, 200) as u16,
            indent: int(Key::EditorIndent, 2).clamp(0, 8) as usize,
            ..EditorSettings::default()
        };
        self.pane.update(cx, |pane, cx| {
            if pane.settings() != &settings {
                pane.apply_settings(settings, window, cx);
            }
            pane.set_read_only(read_only, cx);
        });
    }

    /// The element.
    pub(crate) fn render(&self) -> impl IntoElement {
        self.pane.clone()
    }
}
