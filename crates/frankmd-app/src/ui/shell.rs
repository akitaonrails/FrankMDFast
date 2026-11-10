//! The app shell: one window's root view (ui-spec §2–§4).
//!
//! It owns the window's [`Workspace`] (the single authoritative state,
//! HP-002) and the GUI-free view models of `frankmd-workspace` (layout,
//! focus, overlays, banners, status, quick open, command registry). Views
//! render from them and every user action is a registry command
//! ([`Shell::run_command`]); no domain logic lives here.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use frankmd_config::store::ConfigStore;
use frankmd_config::{EnvSnapshot, Key, Value};
use frankmd_core::Clock;
use frankmd_core::tree::{FileType, TreeNode};
use frankmd_fs::Vault;
use frankmd_i18n::Locale;
use frankmd_theme::OmarchyPaths;
use frankmd_workspace::autosave::{AutosaveEffect, SaveStatus};
use frankmd_workspace::banners::{BannerKind, BannerStack};
use frankmd_workspace::commands::CommandRegistry;
use frankmd_workspace::doc_index::DocIndex;
use frankmd_workspace::drafts::open_draft_storage;
use frankmd_workspace::focus::FocusRegion;
use frankmd_workspace::layout::{LayoutState, PreviewMode, SidebarMode, ViewMode, WidthClass};
use frankmd_workspace::overlay::{OverlayKind, OverlayStack};
use frankmd_workspace::quick_open::{NoteEntry, QuickOpenModel};
use frankmd_workspace::status::{MessageSlot, SaveState};
use frankmd_workspace::{Command, Effect, Event, Workspace};
use gpui_kit::component::input::InputState;
use gpui_kit::*;

use super::appearance::{self, ResolvedTheme};
use super::editor_host::EditorHost;
use super::i18n;
use super::palette::PaletteState;
use super::settings::SettingsState;

/// Everything a shell needs to open a vault. Tests build it over a temp dir.
#[derive(Debug, Clone)]
pub struct ShellConfig {
    /// Notes root.
    pub vault_root: PathBuf,
    /// Per-vault state directory outside the vault (drafts, DocId index), created if missing.
    pub state_dir: PathBuf,
    /// Environment snapshot (config resolution).
    pub env: EnvSnapshot,
    /// Note to open at startup (`fed <path>`), vault-relative or absolute inside the vault.
    pub open: Option<String>,
    /// Time source (autosave timers, status-message TTLs).
    pub clock: Arc<dyn Clock>,
    /// Omarchy locations (`None`: don't follow Omarchy).
    pub omarchy: Option<OmarchyPaths>,
    /// Start the file watchers (vault, `.fed`, Omarchy theme, fonts).
    pub watch: bool,
    /// Locale override (`--locale`).
    pub locale: Option<String>,
    /// Where deleted items go: `None` = the system Trash (ui-spec §5.13);
    /// tests point it at a temp dir.
    pub trash_dir: Option<PathBuf>,
    /// Global settings file (`~/.config/frankmdfast/config.toml`, Settings scope
    /// "All vaults"); `None` disables the layer.
    pub global_config: Option<PathBuf>,
    /// App-wide preferences file (UI scale); `None` keeps them in memory.
    pub app_prefs: Option<PathBuf>,
}

/// `Box<dyn Clock>` over a shared clock (the workspace takes ownership of its clock).
#[derive(Debug)]
struct SharedClock(Arc<dyn Clock>);

impl Clock for SharedClock {
    fn now(&self) -> jiff::Zoned {
        self.0.now()
    }
    fn instant(&self) -> Instant {
        self.0.instant()
    }
}

/// The opened vault.
pub(crate) struct Core {
    pub(crate) ws: Workspace,
    pub(crate) vault: Vault,
}

impl Core {
    fn open(config: &ShellConfig) -> anyhow::Result<Self> {
        crate::app_fs::ensure_private_dir(&config.state_dir)?;
        let mut vault = Vault::open(&config.vault_root, &config.state_dir)?;
        if let Some(dir) = &config.trash_dir {
            vault = vault.with_trash(frankmd_fs::TrashBackend::Directory(dir.clone()));
        }
        let mut store = ConfigStore::open(vault.clone(), config.env.clone());
        if let Some(path) = &config.global_config {
            store = store.with_global(path.clone());
        }
        let drafts = open_draft_storage(&config.state_dir).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let index = DocIndex::open(&config.state_dir);
        let ws = Workspace::new(
            vault.clone(),
            store,
            drafts,
            index,
            Box::new(SharedClock(Arc::clone(&config.clock))),
        );
        Ok(Self { ws, vault })
    }
}

/// Quick open (overlay and docked empty state share it).
pub(crate) struct QuickOpenState {
    pub(crate) model: QuickOpenModel,
    pub(crate) input: Entity<InputState>,
}

/// Help dialog tab (§5.18).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum HelpTab {
    #[default]
    Markdown,
    Keyboard,
    Vim,
}

/// Root view of a window.
pub struct Shell {
    pub(crate) vault_root: PathBuf,
    pub(crate) vault_name: SharedString,
    pub(crate) core: Option<Core>,
    pub(crate) startup_error: Option<SharedString>,
    pub(crate) registry: CommandRegistry,
    pub(crate) layout: LayoutState,
    pub(crate) overlays: OverlayStack,
    pub(crate) banners: BannerStack,
    pub(crate) messages: MessageSlot,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) width: f32,
    pub(crate) root_focus: FocusHandle,
    pub(crate) sidebar_focus: FocusHandle,
    pub(crate) preview_focus: FocusHandle,
    pub(crate) preview: Entity<crate::preview::PreviewPane>,
    pub(crate) editor_font: Option<SharedString>,
    /// The `editor_font` id the last font lookup was for (`None`: none yet).
    pub(crate) editor_font_id: Option<Option<String>>,
    pub(crate) overlay_focus: FocusHandle,
    pub(crate) library_focus: FocusHandle,
    pub(crate) editor: EditorHost,
    pub(crate) explorer: frankmd_workspace::explorer::Explorer,
    pub(crate) ui_state: frankmd_workspace::ui_state::UiStateStore,
    pub(crate) app_prefs: frankmd_workspace::app_prefs::AppPrefsStore,
    pub(crate) history: frankmd_workspace::navigation::History,
    pub(crate) switcher: frankmd_workspace::navigation::RecentSwitcher,
    pub(crate) search: super::search::SearchState,
    pub(crate) files: super::file_dialogs::FileDialogs,
    pub(crate) backlinks: super::sidebar::Backlinks,
    pub(crate) splitter: Option<super::render::Splitter>,
    pub(crate) tree_ui: super::explorer::TreeUi,
    pub(crate) splitter_focus: (FocusHandle, FocusHandle),
    /// Last user-facing error of the current dispatch (dialogs show it inline).
    pub(crate) last_error: Option<frankmd_workspace::command::UserError>,
    /// Media, AI and editor-request wiring (src/dialogs/**).
    pub(crate) media: super::media_host::MediaHost,
    /// Set while a `history.back/forward` open runs (it must not push history).
    pub(crate) navigating: bool,
    pub(crate) palette: PaletteState,
    pub(crate) quick_open: QuickOpenState,
    pub(crate) settings: SettingsState,
    pub(crate) help_tab: HelpTab,
    pub(crate) recovery: Option<super::recovery::RecoveryState>,
    pub(crate) theme: ResolvedTheme,
    pub(crate) omarchy: Option<OmarchyPaths>,
    pub(crate) radius: f32,
    pub(crate) locale_override: Option<Locale>,
    pub(crate) recent_commands: Vec<&'static str>,
    pub(crate) scroll_sync: bool,
    pub(crate) vim: bool,
    pub(crate) autosave_timer: Option<Task<()>>,
    pub(crate) message_timer: Option<Task<()>>,
    pub(crate) watchers: Option<frankmd_watch::Watchers>,
    pub(crate) last_region: FocusRegion,
    pub(crate) window: AnyWindowHandle,
    pub(crate) _subscriptions: Vec<Subscription>,
}

impl Shell {
    /// Open the shell on `config` (does the startup phases of HP-016 in order:
    /// config → theme/locale → workspace → first scan → initial note).
    pub fn new(config: ShellConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let vault_name: SharedString = config
            .vault_root
            .file_name()
            .map_or_else(
                || config.vault_root.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
            .into();
        let (core, startup_error) = match Core::open(&config) {
            Ok(core) => (Some(core), None),
            Err(err) => {
                tracing::error!(%err, "vault unavailable");
                (None, Some(SharedString::from(err.to_string())))
            }
        };

        let (editor, editor_sub) = EditorHost::new(window, cx);
        let qo_placeholder = i18n::t(cx, "quick_open.placeholder");
        let qo_input = cx.new(|cx| InputState::new(window, cx).placeholder(qo_placeholder));
        let qo_sub = cx.subscribe_in(&qo_input, window, |this, input, ev, window, cx| {
            if matches!(ev, gpui_kit::component::input::InputEvent::Change) {
                let q = input.read(cx).value().to_string();
                this.quick_open.model.set_query(&q);
                let _ = window;
                cx.notify();
            }
        });
        let (palette, palette_sub) = PaletteState::new(window, cx);
        let (settings, settings_sub) = SettingsState::new(window, cx);
        let (search, search_subs) = super::search::SearchState::new(window, cx);
        let (files, files_subs) = super::file_dialogs::FileDialogs::new(window, cx);
        let ui_state = if core.is_some() {
            frankmd_workspace::ui_state::UiStateStore::open(&config.state_dir)
        } else {
            frankmd_workspace::ui_state::UiStateStore::in_memory()
        };
        let app_prefs = match &config.app_prefs {
            Some(file) => frankmd_workspace::app_prefs::AppPrefsStore::open(file),
            None => frankmd_workspace::app_prefs::AppPrefsStore::in_memory(),
        };
        // Before the first theme apply, so the first frame is already scaled.
        crate::scale::set(app_prefs.get().ui_scale);
        let media_ctx = core.as_ref().map(|_| crate::dialogs::media::MediaContext {
            vault_root: config.vault_root.clone(),
            state_dir: config.state_dir.clone(),
            env: config.env.clone(),
        });
        let (media, media_subs) = super::media_host::MediaHost::new(media_ctx, window, cx);
        let preview = cx.new(|cx| crate::preview::PreviewPane::new(window, cx));
        let preview_sub = cx.subscribe_in(&preview, window, Self::on_preview_event);

        let locale_override = config.locale.as_deref().and_then(Locale::from_code);
        let mut shell = Self {
            vault_root: config.vault_root.clone(),
            vault_name,
            core,
            startup_error,
            registry: CommandRegistry::builtin(),
            layout: LayoutState::new(),
            overlays: OverlayStack::default(),
            banners: BannerStack::default(),
            messages: MessageSlot::default(),
            clock: Arc::clone(&config.clock),
            width: f32::from(window.viewport_size().width),
            root_focus: cx.focus_handle(),
            sidebar_focus: cx.focus_handle(),
            preview_focus: preview.read(cx).focus_handle(cx),
            preview,
            editor_font: None,
            editor_font_id: None,
            overlay_focus: cx.focus_handle(),
            library_focus: cx.focus_handle(),
            editor,
            explorer: frankmd_workspace::explorer::Explorer::default(),
            ui_state,
            app_prefs,
            history: Default::default(),
            switcher: Default::default(),
            search,
            files,
            backlinks: Default::default(),
            splitter: None,
            tree_ui: Default::default(),
            splitter_focus: (cx.focus_handle(), cx.focus_handle()),
            last_error: None,
            media,
            navigating: false,
            palette,
            quick_open: QuickOpenState {
                model: QuickOpenModel::default(),
                input: qo_input,
            },
            settings,
            help_tab: HelpTab::default(),
            recovery: None,
            theme: appearance::resolve_id(
                None,
                &frankmd_config::ResolvedConfig::new(None, EnvSnapshot::default()),
                None,
            ),
            omarchy: config.omarchy.clone(),
            radius: crate::bridge::theme::DEFAULT_RADIUS,
            locale_override,
            recent_commands: Vec::new(),
            scroll_sync: true,
            vim: false,
            autosave_timer: None,
            message_timer: None,
            watchers: None,
            last_region: FocusRegion::Editor,
            window: window.window_handle(),
            _subscriptions: [editor_sub, qo_sub, palette_sub, settings_sub, preview_sub]
                .into_iter()
                .chain(search_subs)
                .chain(files_subs)
                .chain(media_subs)
                .collect(),
        };
        let requests = shell.subscribe_editor_requests(window, cx);
        shell._subscriptions.push(requests);
        let expanded = shell.ui_state.get().expanded.clone();
        shell.explorer.set_expanded(expanded);
        shell.layout.on_resize(shell.width);
        shell.apply_settings(window, cx);
        shell.preview.update(cx, |p, cx| {
            p.set_vault_root(Some(config.vault_root.clone()), cx)
        });
        if config.watch {
            shell.start_watchers(cx);
            shell.load_hyprland_radius(cx);
            shell.load_editor_font(cx);
        }

        // First tree scan synchronously: the explorer and quick open must be
        // complete on the first frame (the scan is a short readdir walk).
        if let Some(core) = shell.core.as_mut() {
            let effect = core.ws.rescan();
            if let Effect::ScanTree { generation } = effect {
                let tree = core.vault.list_tree().unwrap_or_default();
                shell.dispatch(Command::TreeScanned { generation, tree }, window, cx);
            }
        }
        shell.refresh_quick_open(cx);
        shell.open_initial(config.open.as_deref(), window, cx);
        shell.update_title(window);
        shell
    }

    /// Feed a file-watcher event (vault or `.fed` change) to the workspace;
    /// the watcher loop does this, tests call it directly.
    pub fn external_event(
        &mut self,
        event: frankmd_watch::WatchEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dispatch(Command::External(event), window, cx);
    }

    /// Notes root of this window.
    pub fn vault_root(&self) -> &std::path::Path {
        &self.vault_root
    }

    /// Open a forwarded `fed <path>` note argument in this window.
    pub fn open_argument(&mut self, arg: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_initial(Some(arg), window, cx);
    }

    // ----- startup ------------------------------------------------------------

    // ----- workspace plumbing ----------------------------------------------------

    /// Send one command to the workspace and apply its events and effects.
    pub(crate) fn dispatch(&mut self, cmd: Command, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_mut() else {
            return;
        };
        let (events, effects) = core.ws.handle(cmd);
        for event in events {
            self.apply_event(event, window, cx);
        }
        for effect in effects {
            self.run_effect(effect, cx);
        }
        cx.notify();
    }

    fn apply_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::DocOpened { doc, path, text } => {
                self.editor.set_document(Some(doc), &text, window, cx);
                self.render_preview_now(cx);
                self.layout.note_opened();
                self.on_note_opened(path.as_str(), cx);
                self.banners.clear(BannerKind::ChangedOnDisk);
                self.banners.clear(BannerKind::DeletedOnDisk);
                self.banners.clear(BannerKind::ContentLoss);
                let read_only = self
                    .core
                    .as_ref()
                    .and_then(|c| c.ws.open_doc())
                    .is_some_and(|d| d.read_only);
                self.banners.set(BannerKind::ReadOnly, read_only);
                self.update_title(window);
            }
            Event::DocClosed { .. } | Event::DocDeleted { .. } => {
                self.on_note_closed(cx);
                self.editor.set_document(None, "", window, cx);
                self.render_preview_now(cx);
                for kind in [
                    BannerKind::ChangedOnDisk,
                    BannerKind::DeletedOnDisk,
                    BannerKind::ContentLoss,
                    BannerKind::ReadOnly,
                ] {
                    self.banners.clear(kind);
                }
                self.update_title(window);
            }
            Event::DocReloaded { text, .. } => {
                self.editor.set_text(&text, window, cx);
                self.render_preview_now(cx);
            }
            Event::DocConflict { .. } => self.banners.raise(BannerKind::ChangedOnDisk),
            Event::DocDeletedOnDisk { .. } => self.banners.raise(BannerKind::DeletedOnDisk),
            Event::DocSaved { .. } => self.update_title(window),
            Event::Remapped(remaps) => {
                self.on_remapped(&remaps, cx);
                self.update_title(window);
            }
            Event::TreeChanged => self.on_tree_changed(cx),
            Event::Reveal { path } => self.reveal_in_tree(path.as_str()),
            Event::SettingsChanged => {
                self.apply_settings(window, cx);
                self.media_config_changed(cx);
            }
            Event::Autosave(effect) => self.apply_autosave(effect, window, cx),
            Event::System(ev) => self.apply_system(ev, window, cx),
            Event::Error(err) => {
                if self.last_error.is_none() {
                    self.last_error = Some(err.clone());
                }
                let args: Vec<(&str, &str)> =
                    err.args.iter().map(|(k, v)| (*k, v.as_str())).collect();
                let text = i18n::t_args(cx, err.key, &args);
                self.show_message(&text, true, cx);
            }
        }
    }

    fn apply_autosave(
        &mut self,
        effect: AutosaveEffect,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match effect {
            AutosaveEffect::Status(SaveStatus::DraftStorageError) => {
                self.banners.raise(BannerKind::DraftStorageFailed)
            }
            AutosaveEffect::Status(SaveStatus::Saved) => {
                self.banners.clear(BannerKind::DraftStorageFailed)
            }
            AutosaveEffect::ContentLossBanner(shown) => {
                self.banners.set(BannerKind::ContentLoss, shown)
            }
            AutosaveEffect::OfflineChanged(offline) => {
                self.banners.set(BannerKind::VaultUnreachable, offline)
            }
            AutosaveEffect::SetEditorText(text)
            | AutosaveEffect::LoadEditorText { content: text, .. } => {
                self.editor.set_text(&text, window, cx);
            }
            AutosaveEffect::OpenRecovery(request) => self.open_recovery(request, window, cx),
            AutosaveEffect::Status(_)
            | AutosaveEffect::Save(_)
            | AutosaveEffect::UnloadGuard(_)
            | AutosaveEffect::ConfigSaved => {}
        }
    }

    pub(crate) fn apply_system(
        &mut self,
        ev: frankmd_watch::WatchEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use frankmd_watch::WatchEvent;
        match ev {
            WatchEvent::OmarchyThemeChanged => {
                if self.theme.id == appearance::OMARCHY || self.theme.omarchy_name.is_some() {
                    match appearance::omarchy_tokens(self.omarchy.as_ref()) {
                        Some((name, tokens)) => {
                            self.theme = ResolvedTheme {
                                tokens,
                                id: appearance::OMARCHY.to_owned(),
                                omarchy_name: Some(name),
                            };
                            let tokens = self.theme.tokens.clone();
                            self.apply_tokens(&tokens, cx);
                        }
                        None => {
                            let text = i18n::t(cx, "status.theme_sync_failed");
                            self.show_message(&text, true, cx);
                        }
                    }
                }
            }
            WatchEvent::SystemLanguageChanged => self.apply_locale(cx),
            WatchEvent::Error(_) => {
                let text = i18n::t(cx, "status.watcher_failed");
                self.show_message(&text, true, cx);
            }
            WatchEvent::FontsChanged => self.load_editor_font(cx),
            WatchEvent::ConfigChanged | WatchEvent::VaultChanged { .. } => {}
        }
        let _ = window;
    }

    pub(crate) fn run_effect(&mut self, effect: Effect, cx: &mut Context<Self>) {
        match effect {
            Effect::ScanTree { generation } => {
                let Some(core) = self.core.as_ref() else {
                    return;
                };
                let vault = core.vault.clone();
                self.spawn_scan(vault, generation, cx);
            }
            Effect::Render { .. } => self.render_preview_now(cx),
            Effect::WakeAt(at) => {
                let delay = at.saturating_duration_since(self.clock.instant());
                self.autosave_timer = Some(self.spawn_tick(delay, cx));
            }
        }
    }

    /// The editor reported a user edit.
    pub(crate) fn editor_changed(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self
            .core
            .as_ref()
            .and_then(|c| c.ws.open_doc())
            .map(|d| d.id)
        else {
            return;
        };
        self.dispatch(Command::Edited { doc, text }, window, cx);
    }

    /// Rebuild quick open's candidates from the tree and the shared recents.
    pub(crate) fn refresh_quick_open(&mut self, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else {
            return;
        };
        fn walk(nodes: &[TreeNode], out: &mut Vec<NoteEntry>) {
            for node in nodes {
                match node {
                    TreeNode::Folder { children, .. } => walk(children, out),
                    TreeNode::File {
                        path,
                        file_type: FileType::Markdown,
                        mtime,
                        ..
                    } => {
                        out.push(NoteEntry {
                            path: path.as_str().to_owned(),
                            mtime: *mtime,
                        });
                    }
                    TreeNode::File { .. } => {}
                }
            }
        }
        let mut notes = Vec::new();
        walk(&core.ws.snapshot().tree, &mut notes);
        let query = self.quick_open.input.read(cx).value().to_string();
        self.quick_open.model = QuickOpenModel::new(notes, core.ws.recent_paths());
        self.quick_open.model.set_query(&query);
    }

    /// Number of notes in the vault (empty state header).
    pub(crate) fn note_count(&self) -> usize {
        fn count(nodes: &[TreeNode]) -> usize {
            nodes
                .iter()
                .map(|n| match n {
                    TreeNode::Folder { children, .. } => count(children),
                    TreeNode::File { .. } => 1,
                })
                .sum()
        }
        self.core
            .as_ref()
            .map_or(0, |c| count(&c.ws.snapshot().tree))
    }

    /// Re-read `.fed`-backed preferences (startup, our writes, external edits).
    pub(crate) fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(core) = self.core.as_ref() else {
            self.apply_locale(cx);
            let tokens = self.theme.tokens.clone();
            self.apply_tokens(&tokens, cx);
            return;
        };
        let config = core.ws.snapshot().config;
        let read_only = core.ws.open_doc().is_some_and(|d| d.read_only);
        let bool_of = |key: Key, default: bool| {
            config
                .value(key)
                .and_then(|v| v.as_bool())
                .unwrap_or(default)
        };
        self.layout.sidebar_visible = bool_of(Key::SidebarVisible, true);
        self.layout.view_mode =
            ViewMode::parse(config.value(Key::ViewMode).as_ref().and_then(Value::as_str));
        self.layout.typewriter = bool_of(Key::TypewriterMode, false);
        self.scroll_sync = bool_of(Key::ScrollSync, true);
        self.vim = bool_of(Key::VimMode, false);
        if self.palette.preview_theme.is_none() {
            self.theme = appearance::resolve(&config, self.omarchy.as_ref());
            let tokens = self.theme.tokens.clone();
            self.apply_tokens(&tokens, cx);
        }
        self.apply_locale(cx);
        // A new `editor_font` (Settings or a hand edit of `.fed`) needs a new
        // fontconfig lookup; it re-applies the settings when done.
        let font_id = config
            .value(Key::EditorFont)
            .and_then(|v| v.as_str().map(str::to_owned));
        if self.editor_font_id.as_ref() != Some(&font_id) {
            self.load_editor_font(cx);
        }
        self.editor
            .apply_config(&config, self.editor_font.clone(), read_only, window, cx);
        let int = |key: Key, default: i64| {
            config
                .value(key)
                .and_then(|v| v.as_int())
                .unwrap_or(default)
        };
        let zoom = u16::try_from(int(Key::PreviewZoom, 100).clamp(50, 200)).unwrap_or(100);
        let width = u16::try_from(int(Key::EditorWidth, 72).clamp(40, 200)).unwrap_or(72);
        self.preview.update(cx, |p, cx| {
            p.set_zoom(zoom, cx);
            p.set_text_width(width, cx);
        });
        cx.notify();
    }

    /// Apply a palette to gpui-kit's theme and the preview.
    pub(crate) fn apply_tokens(
        &mut self,
        tokens: &frankmd_theme::UiTokens,
        cx: &mut Context<Self>,
    ) {
        crate::bridge::theme::apply(tokens, self.radius, cx);
        self.preview.update(cx, |p, cx| p.set_theme(tokens, cx));
    }

    /// Hand the editor text to the preview (it parses off-thread, generation-tagged).
    pub(crate) fn render_preview_now(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.active_path() else {
            self.preview
                .update(cx, |p, cx| p.set_markdown("", None, cx));
            return;
        };
        if !path.ends_with(".md") {
            self.preview.update(cx, |p, cx| p.set_not_markdown(cx));
            return;
        }
        let text = self.editor.text(cx);
        let base = std::path::Path::new(&path)
            .parent()
            .map(|p| self.vault_root.join(p));
        self.preview
            .update(cx, |p, cx| p.set_markdown(text, base, cx));
    }

    fn on_preview_event(
        &mut self,
        _pane: &Entity<crate::preview::PreviewPane>,
        ev: &crate::preview::PreviewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::preview::PreviewEvent;
        match ev {
            PreviewEvent::VisibleSourceLine(line) => {
                if self.scroll_sync {
                    let line = (*line as usize).saturating_sub(1);
                    self.editor
                        .pane
                        .update(cx, |p, cx| p.sync_to_line(line, cx));
                }
            }
            PreviewEvent::ToggleTask { line, .. } => {
                self.toggle_task_line(*line as usize, window, cx)
            }
            PreviewEvent::OpenWikiLink { target } => self.open_wikilink(target, window, cx),
            PreviewEvent::OpenNote { path } => {
                let rel = path
                    .strip_prefix(&self.vault_root)
                    .ok()
                    .and_then(|p| p.to_str())
                    .map(str::to_owned);
                if let Some(rel) = rel {
                    self.open_path(&rel, window, cx);
                }
            }
            PreviewEvent::ZoomChanged(zoom) => {
                self.update_settings(
                    vec![(Key::PreviewZoom, Some(Value::Int(i128::from(*zoom))))],
                    window,
                    cx,
                );
            }
        }
    }

    /// Preview checkbox: toggle the task marker on 1-based `line` through the
    /// editor (one undo step) so the edit reaches the workspace like typing.
    fn toggle_task_line(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.text(cx);
        let mut start = 0;
        for (ix, l) in text.split_inclusive('\n').enumerate() {
            if ix + 1 == line {
                let body = l.trim_end_matches(['\n', '\r']);
                if let Some(toggled) = frankmd_core::text::task::toggle_task_marker(body) {
                    let range = start..start + body.len();
                    let caret = range.start + toggled.len();
                    let edit = crate::editor::Edit {
                        range,
                        text: toggled,
                        selection: caret..caret,
                    };
                    self.editor
                        .pane
                        .update(cx, |p, cx| p.apply_edit(edit, window, cx));
                }
                return;
            }
            start += l.len();
        }
    }

    /// Every note path of the tree.
    pub(crate) fn quick_open_candidates(&self) -> Vec<String> {
        fn walk(nodes: &[TreeNode], out: &mut Vec<String>) {
            for node in nodes {
                match node {
                    TreeNode::Folder { children, .. } => walk(children, out),
                    TreeNode::File {
                        path,
                        file_type: FileType::Markdown,
                        ..
                    } => out.push(path.as_str().to_owned()),
                    TreeNode::File { .. } => {}
                }
            }
        }
        let mut out = Vec::new();
        if let Some(core) = self.core.as_ref() {
            walk(&core.ws.snapshot().tree, &mut out);
        }
        out
    }

    /// The UI language: `--locale`, then `.fed`/`FRANKMD_LOCALE`, then the system.
    pub(crate) fn apply_locale(&mut self, cx: &mut Context<Self>) {
        let locale = self
            .locale_override
            .unwrap_or_else(|| self.configured_locale());
        i18n::set_locale(locale, cx);
        let catalog = i18n::current(cx);
        self.preview.update(cx, |p, cx| p.set_catalog(catalog, cx));
    }

    /// Locale from config, else the system's.
    pub(crate) fn configured_locale(&self) -> Locale {
        // The schema default ("en") is not a choice: without an explicit
        // `.fed` / global / ENV value, Omarchy or `LANG` decide.
        let configured = self.core.as_ref().and_then(|c| {
            let resolved = c.ws.snapshot().config.get(Key::Locale);
            if resolved.source == frankmd_config::Provenance::Default {
                return None;
            }
            resolved
                .value
                .and_then(|v| v.as_str().and_then(Locale::from_code))
        });
        configured.unwrap_or_else(|| self.system_locale())
    }

    /// Omarchy's language (`~/.config/omarchy/language`, live-watched), then
    /// `LC_ALL` / `LC_MESSAGES` / `LANG`, else English (OM §4.3 steps 5–7).
    ///
    /// Omarchy comes first because it changes live; `LANG` only follows it
    /// after a restart.
    pub(crate) fn system_locale(&self) -> Locale {
        let env = self
            .core
            .as_ref()
            .map(|c| c.ws.snapshot().config.env().clone())
            .unwrap_or_default();
        let omarchy = env
            .present("HOME")
            .and_then(|home| omarchy_language(std::path::Path::new(home)));
        omarchy
            .or_else(|| {
                ["LC_ALL", "LC_MESSAGES", "LANG"]
                    .iter()
                    .find_map(|name| env.present(name).and_then(Locale::from_system))
            })
            .unwrap_or(Locale::En)
    }

    /// Change the interface size (Settings → Appearance) and redraw everything.
    pub(crate) fn set_ui_scale(
        &mut self,
        percent: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.app_prefs.set_ui_scale(percent);
        crate::scale::set(self.app_prefs.get().ui_scale);
        // Text sizes live in the theme; preview metrics are derived on apply.
        self.apply_settings(window, cx);
        cx.refresh_windows();
    }

    /// Write `.fed` keys through the workspace.
    pub(crate) fn update_settings(
        &mut self,
        changes: Vec<(Key, Option<Value>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dispatch(Command::UpdateSettings { changes }, window, cx);
    }

    // ----- navigation -------------------------------------------------------------

    /// Text of the editor (what the workspace last received as an edit).
    pub fn editor_text(&self, cx: &App) -> String {
        self.editor.text(cx)
    }

    /// Current status-line message, if not expired.
    pub fn status_message(&self) -> Option<String> {
        self.messages
            .current(self.clock.instant())
            .map(|m| m.text.clone())
    }

    /// Path of the active note.
    pub fn active_path(&self) -> Option<String> {
        self.core
            .as_ref()
            .and_then(|c| c.ws.open_doc())
            .map(|d| d.path.as_str().to_owned())
    }

    fn update_title(&self, window: &mut Window) {
        let title = match self.active_path() {
            Some(path) => {
                let name = path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&path)
                    .trim_end_matches(".md")
                    .to_owned();
                format!("{name} — FrankMDFast")
            }
            None => format!("FrankMDFast — {}", self.vault_name),
        };
        window.set_window_title(&title);
    }

    // ----- status -----------------------------------------------------------------

    /// Show a status-line message (2 s info / 5 s error).
    pub(crate) fn show_message(&mut self, text: &str, error: bool, cx: &mut Context<Self>) {
        let now = self.clock.instant();
        self.messages.show(text, error, now);
        let delay = self
            .messages
            .next_deadline()
            .map(|d| d.saturating_duration_since(now))
            .unwrap_or_default();
        self.message_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        }));
        cx.notify();
    }

    /// Current save-state slot.
    pub fn save_state(&self) -> SaveState {
        match self.core.as_ref() {
            Some(core) => {
                let autosave = core.ws.autosave();
                SaveState::from_autosave(
                    autosave.status(),
                    autosave.is_saving(),
                    core.ws.snapshot().dirty,
                )
            }
            None => SaveState::Clean,
        }
    }

    // ----- layout & focus -----------------------------------------------------------

    /// Layout view model.
    pub fn layout(&self) -> &LayoutState {
        &self.layout
    }

    /// Open overlay.
    pub fn overlay(&self) -> Option<OverlayKind> {
        self.overlays.current().cloned()
    }

    /// Visible banners, top to bottom.
    pub fn banners(&self) -> Vec<BannerKind> {
        self.banners.visible()
    }

    /// Raise a banner for a condition the workspace reports out of band
    /// (vault reachability probe, storage fallback) or, in tests, to drive
    /// the banner wiring directly.
    pub fn raise_banner(&mut self, kind: BannerKind, cx: &mut Context<Self>) {
        self.banners.raise(kind);
        cx.notify();
    }

    /// Current pane arrangement.
    pub(crate) fn arrangement(&self) -> frankmd_workspace::layout::PaneArrangement {
        self.layout.arrangement(self.width)
    }

    pub(crate) fn on_width(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.width = width;
        self.layout.on_resize(width);
        self.ensure_focus_visible(window, cx);
    }

    /// The region that holds keyboard focus.
    pub fn focused_region(&self, window: &Window, cx: &App) -> Option<FocusRegion> {
        if self.sidebar_focus.contains_focused(window, cx) {
            Some(FocusRegion::Sidebar)
        } else if self.library_focus.contains_focused(window, cx)
            || self.editor.focus_handle(cx).contains_focused(window, cx)
            || (self.active_path().is_none()
                && self
                    .quick_open
                    .input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
                && self.overlays.current().is_none())
        {
            Some(FocusRegion::Editor)
        } else if self.preview_focus.contains_focused(window, cx) {
            Some(FocusRegion::Preview)
        } else {
            None
        }
    }

    /// Move focus to `region` (opening what it needs: the narrow drawer, a hidden preview).
    pub(crate) fn focus_region(
        &mut self,
        region: FocusRegion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match region {
            FocusRegion::Sidebar => {
                if self.layout.width_class == WidthClass::Narrow {
                    self.layout.drawer_open = true;
                } else if !self.layout.sidebar_visible
                    || self.arrangement().sidebar == SidebarMode::Hidden
                {
                    self.layout.sidebar_visible = true;
                }
                self.sidebar_focus.focus(window, cx);
            }
            FocusRegion::Editor | FocusRegion::StatusLine => {
                if self.layout.media_library {
                    // The library focuses its own grid; keep it there.
                    if !self.library_focus.contains_focused(window, cx) {
                        self.library_focus.focus(window, cx);
                    }
                } else if self.active_path().is_some() {
                    self.editor.focus(window, cx);
                } else {
                    let input = self.quick_open.input.clone();
                    input.update(cx, |input, cx| input.focus(window, cx));
                }
            }
            FocusRegion::Preview => {
                if self.arrangement().preview == PreviewMode::Hidden {
                    self.layout.toggle_preview();
                }
                self.preview_focus.focus(window, cx);
            }
        }
        self.last_region = region;
        cx.notify();
    }

    /// After a layout change: if the focused region disappeared, move to its fallback.
    pub(crate) fn ensure_focus_visible(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlays.current().is_some() {
            return;
        }
        let arrangement = self.arrangement();
        let region = self.focused_region(window, cx).unwrap_or(self.last_region);
        if !region.is_visible(&arrangement, false) {
            let to = region.fallback(&arrangement);
            self.focus_region(to, window, cx);
        }
    }

    // ----- overlays ---------------------------------------------------------------

    // ----- commands -----------------------------------------------------------------
}

impl Shell {}

/// The FrankMD locale for Omarchy's language choice: the first line of
/// `<home>/.config/omarchy/language`. `None` when the file is missing or the
/// language has no FrankMD translation (then `LANG` decides).
fn omarchy_language(home: &std::path::Path) -> Option<Locale> {
    let text = std::fs::read_to_string(home.join(".config/omarchy/language")).ok()?;
    Locale::from_system(text.lines().next()?.trim())
}

#[cfg(test)]
mod tests {
    use super::{Locale, omarchy_language};

    #[allow(clippy::disallowed_methods)] // test fixture setup
    fn home_with_language(code: Option<&str>) -> tempfile::TempDir {
        let home = tempfile::tempdir().expect("tempdir");
        if let Some(code) = code {
            let dir = home.path().join(".config/omarchy");
            std::fs::create_dir_all(&dir).expect("mkdir");
            std::fs::write(dir.join("language"), format!("{code}\n")).expect("write");
        }
        home
    }

    #[test]
    fn omarchy_language_maps_registry_codes() {
        for (code, locale) in [
            ("pt-BR", Locale::PtBr),
            ("pt", Locale::PtPt),
            ("es-MX", Locale::Es),
            ("ja", Locale::Ja),
        ] {
            let home = home_with_language(Some(code));
            assert_eq!(omarchy_language(home.path()), Some(locale), "{code}");
        }
    }

    #[test]
    fn missing_or_untranslated_omarchy_language_defers_to_lang() {
        assert_eq!(omarchy_language(home_with_language(None).path()), None);
        assert_eq!(
            omarchy_language(home_with_language(Some("de")).path()),
            None
        );
    }
}
