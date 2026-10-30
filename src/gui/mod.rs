//! The window (egui / eframe).
//!
//! * `mod.rs` — application state and the frame layout
//! * `actions.rs` — what happens on clicks: tasks, vault, jobs, loaders
//! * `input.rs` — navigation history, keyboard shortcuts, mouse conveniences
//! * `dialogs.rs` — confirmation and encryption dialogs
//! * `views/` — one module per screen

mod actions;
mod background;
mod dialogs;
mod input;
mod manage;
mod tasks;
#[cfg(test)]
mod tests;
mod theme;
mod views;
mod widgets;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui::{self, Align, CornerRadius, Frame, Layout, Margin, Stroke, Ui};

use crate::config::{Appearance, Config, ConfigNotice, Loaded, Schedule};
use crate::engine::backup::BackupReport;
use crate::engine::crypto::VaultKey;
use crate::engine::plan::{BackupPlan, RestorePlan};
use crate::engine::restore::RestoreReport;
use crate::engine::snapshots::SnapshotInfo;
use crate::engine::vault::VaultHeader;
use crate::error::EngineError;
use crate::i18n::Lang;
use crate::logging::LogBuffer;
use crate::paths::AppPaths;
use crate::platform::{self, apps};
use crate::state::State;
use background::Background;
use tasks::{Job, Task};
use theme::palette;
use widgets::{ButtonKind, COLUMN, NoticeKind};

/// How the window starts.
#[derive(Debug, Clone)]
pub struct StartOptions {
    /// Start in the notification area without showing the window.
    pub hidden: bool,
    /// Name that lets a second start find this instance.
    pub instance_key: String,
    /// Browse the backups at this path instead of the normal window.
    pub viewer: Option<PathBuf>,
}

pub fn run(
    paths: AppPaths,
    loaded: Loaded,
    log: LogBuffer,
    options: StartOptions,
) -> anyhow::Result<()> {
    let compatibility = loaded.config.advanced.compatibility_graphics
        || std::env::var("AETERNAVAULT_RENDERER").is_ok_and(|v| v.eq_ignore_ascii_case("glow"));

    #[cfg(feature = "wgpu")]
    if !compatibility {
        match launch(
            eframe::Renderer::Wgpu,
            paths.clone(),
            loaded.clone(),
            log.clone(),
            options.clone(),
        ) {
            Ok(()) => return Ok(()),
            // Some graphics drivers fail with Direct3D 12 or Vulkan; OpenGL usually works.
            Err(err) => tracing::warn!("the default renderer failed, trying OpenGL: {err}"),
        }
    }
    let _ = compatibility;
    launch(eframe::Renderer::Glow, paths, loaded, log, options).map_err(|e| anyhow::anyhow!("{e}"))
}

fn launch(
    renderer: eframe::Renderer,
    paths: AppPaths,
    loaded: Loaded,
    log: LogBuffer,
    options: StartOptions,
) -> Result<(), eframe::Error> {
    let (size, min_size) = window_sizes();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("AeternaVault")
        .with_app_id("aeternavault")
        .with_inner_size(size)
        .with_min_inner_size(min_size)
        .with_visible(!options.hidden);
    match eframe::icon_data::from_png_bytes(include_bytes!(
        "../../assets/icon/aeternavault-256.png"
    )) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(err) => tracing::warn!("window icon could not be loaded: {err}"),
    }

    #[allow(unused_mut)]
    let mut native = eframe::NativeOptions {
        viewport,
        centered: true,
        renderer,
        ..Default::default()
    };
    #[cfg(feature = "wgpu")]
    {
        native.wgpu_options.wgpu_setup = wgpu_setup();
    }
    tracing::info!(?renderer, ?size, hidden = options.hidden, "opening window");

    eframe::run_native(
        "AeternaVault",
        native,
        Box::new(move |cc| {
            theme::install_fonts(&cc.egui_ctx, &loaded.config.fonts);
            theme::apply(&cc.egui_ctx, loaded.config.appearance);
            let mut app = AeternaApp::new(&cc.egui_ctx, paths, loaded, log);
            match &options.viewer {
                Some(path) => app.enter_viewer(&cc.egui_ctx, path),
                None => {
                    manage::clean_open_folder();
                    app.start_background(&cc.egui_ctx, &options);
                }
            }
            Ok(Box::new(app))
        }),
    )
}

/// Start and minimum size in points, limited to the screen's work area so the
/// window never opens larger than the screen.
fn window_sizes() -> ([f32; 2], [f32; 2]) {
    const WANTED: [f32; 2] = [1240.0, 900.0];
    const MINIMUM: [f32; 2] = [820.0, 600.0];
    let Some((width, height)) = platform::work_area_points() else {
        return (WANTED, MINIMUM);
    };
    // Leave room for the title bar and a little air around the window.
    let (max_w, max_h) = (width * 0.92, height * 0.90);
    let size = [WANTED[0].min(max_w), WANTED[1].min(max_h)];
    let min = [MINIMUM[0].min(size[0]), MINIMUM[1].min(size[1])];
    (size, min)
}

/// Direct3D 12 on Windows, on the integrated (power-saving) graphics chip:
/// hybrid laptops otherwise wake the dedicated GPU, which can flicker at start.
/// The usual `WGPU_BACKEND` / `WGPU_POWER_PREF` environment variables still
/// override this.
#[cfg(feature = "wgpu")]
fn wgpu_setup() -> eframe::egui_wgpu::WgpuSetup {
    use eframe::wgpu;
    let mut setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    if cfg!(windows) && wgpu::Backends::from_env().is_none() {
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
    }
    if wgpu::PowerPreference::from_env().is_none() {
        setup.power_preference = wgpu::PowerPreference::LowPower;
    }
    eframe::egui_wgpu::WgpuSetup::CreateNew(setup)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Backup,
    Restore,
    Backups,
    Jobs,
    Apps,
    Notifications,
    Settings,
    Activity,
}

impl View {
    /// In the order of the tabs.
    pub const ALL: [View; 8] = [
        View::Backup,
        View::Restore,
        View::Backups,
        View::Jobs,
        View::Apps,
        View::Notifications,
        View::Settings,
        View::Activity,
    ];
}

pub enum Screen {
    Main,
    Working,
    Browse(Box<views::browse::BrowseState>),
    Done(Box<Done>),
}

// Lives boxed inside `Screen::Done`, so the size difference does not matter.
#[allow(clippy::large_enum_variant)]
pub enum Done {
    Backup(Result<BackupReport, String>),
    Restore(Result<RestoreReport, String>),
    Verify(Result<crate::engine::verify::VerifyReport, String>),
    Delete(Result<crate::engine::manage::DeleteReport, String>),
    Transfer(Result<crate::engine::manage::TransferReport, String>),
    Extract(Result<(crate::engine::manage::ExtractReport, PathBuf), String>),
}

/// Something waiting for the user's confirmation.
pub enum Pending {
    Backup(Box<BackupPlan>),
    Restore(Box<RestorePlan>),
    Delete(Vec<SnapshotInfo>),
    Transfer(Box<SnapshotInfo>, PathBuf),
    Extract {
        snapshot: Box<SnapshotInfo>,
        /// `None`: everything.
        files: Option<Vec<crate::engine::snapshots::StoredFile>>,
        target: PathBuf,
    },
}

/// Selection in the Backups view.
#[derive(Default)]
pub struct ManageUi {
    /// Qualified ids of the ticked backups.
    pub checked: HashSet<String>,
    pub extract_target: Option<PathBuf>,
    /// The backup a running check, move or copy works on, and where to (for the history).
    pub current: Option<(String, PathBuf)>,
    /// Cached forecast of the retention rules: (input fingerprint, forecast).
    pub forecast: Option<(u64, std::rc::Rc<RetentionForecast>)>,
}

/// When each backup is expected to be removed by the retention rules.
pub struct RetentionForecast {
    pub removals: Vec<(String, crate::engine::retention::Removal)>,
    /// No automatic backup is switched on, so one backup a day is assumed.
    pub assumed_daily: bool,
}

pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
    /// When it disappears on its own; pointing at it holds it.
    pub until: Instant,
}

impl Notice {
    fn lifetime(kind: &NoticeKind) -> std::time::Duration {
        std::time::Duration::from_secs(match kind {
            NoticeKind::Info | NoticeKind::Success => 6,
            NoticeKind::Warning => 10,
            NoticeKind::Error => 15,
        })
    }
}

/// Folder path with (bytes, files), or `None` if the folder is missing.
type SizeLoad = (PathBuf, Option<(u64, u64)>);
type SnapshotLoad = (Result<Vec<SnapshotInfo>, EngineError>, Option<u64>);

#[derive(Default)]
pub struct RestoreUi {
    pub selected: Option<String>,
    pub to_folder: bool,
    pub folder: Option<PathBuf>,
    /// Folders of the selected backup that are left out (see `RestoreOptions::skip`).
    pub skip: HashSet<String>,
}

/// An application found on this computer, with its data folders.
#[derive(Debug, Clone)]
pub struct DetectedApp {
    pub id: String,
    pub category: apps::Category,
    pub folders: Vec<PathBuf>,
    /// Size of all folders, once measured.
    pub bytes: Option<u64>,
}

#[derive(Default)]
pub struct AppsUi {
    pub detected: Option<Vec<DetectedApp>>,
    pub job: Option<Job<Vec<DetectedApp>>>,
    pub search: String,
}

#[derive(Default)]
pub struct SettingsUi {
    pub exclude_text: String,
    pub invalid_patterns: Vec<String>,
}

/// Entries of one folder in the "choose contents" tree.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Default)]
pub struct TreeUi {
    /// Sources (by path) whose tree is open.
    pub expanded_sources: HashSet<PathBuf>,
    /// Open folders: `(source path, relative folder)`.
    pub open_dirs: HashSet<(PathBuf, String)>,
    pub listings: HashMap<PathBuf, Vec<TreeEntry>>,
    /// A source to scroll into view once (after adding it).
    pub reveal: Option<PathBuf>,
}

/// A step that was waiting for the vault to be unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterUnlock {
    Backup,
    EnableEncryption,
    /// Reading backups: browse, open, copy out, restore.
    Read,
    ChangePassphrase,
    ReplaceRecovery,
    Remember,
}

pub enum VaultDialog {
    Create {
        passphrase: String,
        repeat: String,
        remember: bool,
        options: crate::engine::vault::VaultOptions,
        error: Option<String>,
    },
    /// Enter a recovery key to see whether it still unlocks the backups.
    TestRecovery {
        secret: String,
        result: Option<Result<(), String>>,
    },
    ShowRecovery {
        key: String,
        confirmed: bool,
        /// egui time of the last click on "Copy", for a short confirmation.
        copied_at: Option<f64>,
    },
    Unlock {
        secret: String,
        remember: bool,
        error: Option<String>,
        then: AfterUnlock,
    },
    Change {
        passphrase: String,
        repeat: String,
        error: Option<String>,
    },
}

/// How long reading encrypted backups stays allowed after the passphrase was
/// entered, counted from the last time something was read.
pub const READ_ACCESS: std::time::Duration = std::time::Duration::from_secs(15 * 60);

#[derive(Default)]
pub struct VaultUi {
    /// The vault at the current destination, if one exists.
    pub header: Option<VaultHeader>,
    /// The key for backing up. It may come from the remembered key.
    pub key: Option<VaultKey>,
    pub remembered: bool,
    pub dialog: Option<VaultDialog>,
    /// File names and contents of encrypted backups may be shown until then.
    /// Only entering the passphrase or recovery key grants this — never the
    /// remembered key.
    pub read_until: Option<Instant>,
}

impl VaultUi {
    pub fn can_read(&self) -> bool {
        self.key.is_some() && self.read_until.is_some_and(|until| until > Instant::now())
    }
}

/// A schedule being created or changed in the dialog.
pub struct ScheduleEditor {
    pub schedule: Schedule,
    pub is_new: bool,
}

#[derive(Default)]
pub struct ScheduleUi {
    pub editor: Option<ScheduleEditor>,
}

pub struct AeternaApp {
    pub paths: AppPaths,
    pub config: Config,
    pub state: State,
    pub catalog: apps::Catalog,
    pub lang: Lang,
    pub view: View,
    pub screen: Screen,
    pub task: Option<Task>,
    pub pending: Option<Pending>,
    pub notices: Vec<Notice>,
    pub log: LogBuffer,
    /// The activity history, oldest first (see [`crate::history`]).
    pub history: Vec<crate::history::Entry>,
    pub computer: String,

    pub sizes: HashMap<PathBuf, Option<(u64, u64)>>,
    sizes_job: Option<Job<SizeLoad>>,
    pub snapshots: Option<Result<Vec<SnapshotInfo>, EngineError>>,
    snapshots_job: Option<Job<SnapshotLoad>>,
    pub destination_free: Option<u64>,

    pub restore: RestoreUi,
    pub apps: AppsUi,
    pub settings: SettingsUi,
    pub tree: TreeUi,
    pub vault: VaultUi,
    pub schedule: ScheduleUi,
    pub manage: ManageUi,
    manage_job: Option<Job<Option<Result<crate::engine::manage::DeleteReport, EngineError>>>>,
    pub background: Option<Background>,
    /// Opened by double-click on encrypted backups: only browse and restore
    /// the backups at this folder; the configuration is not changed.
    pub viewer: Option<PathBuf>,
    pub nav: input::Navigation,
    pub updater: crate::update::Updater,
    config_dirty: bool,
}

impl AeternaApp {
    fn new(ctx: &egui::Context, paths: AppPaths, loaded: Loaded, log: LogBuffer) -> Self {
        let lang = Lang::resolve(loaded.config.language);
        let config_dir = paths
            .config_file
            .parent()
            .map(PathBuf::from)
            .unwrap_or_default();
        let state = State::load(&paths.config_file);
        let updater = {
            let ctx = ctx.clone();
            crate::update::Updater::new(
                loaded.config.updates.check_automatically,
                config_dir.join("last-update-check"),
                move || ctx.request_repaint(),
            )
        };
        let mut app = Self {
            updater,
            settings: SettingsUi {
                exclude_text: loaded.config.exclude.join("\n"),
                invalid_patterns: Vec::new(),
            },
            catalog: apps::Catalog::load(&config_dir),
            state,
            paths,
            config: loaded.config,
            lang,
            view: View::Backup,
            screen: Screen::Main,
            task: None,
            pending: None,
            notices: Vec::new(),
            log,
            history: Vec::new(),
            computer: platform::computer_name(),
            sizes: HashMap::new(),
            sizes_job: None,
            snapshots: None,
            snapshots_job: None,
            destination_free: None,
            restore: RestoreUi::default(),
            apps: AppsUi::default(),
            tree: TreeUi::default(),
            vault: VaultUi::default(),
            schedule: ScheduleUi::default(),
            manage: ManageUi::default(),
            manage_job: None,
            background: None,
            viewer: None,
            nav: input::Navigation::default(),
            config_dirty: false,
        };

        ctx.set_zoom_factor(app.config.zoom_factor());
        if let Some(notice) = loaded.notice {
            app.config_notice(notice);
        }
        // Applications chosen in AeternaVault 0.4 and older become folders.
        if !app.config.legacy_apps.is_empty() {
            let known = platform::known_paths::KnownPaths::current();
            let added = app.config.take_legacy_apps(&app.catalog, &known, lang);
            if !added.is_empty() {
                app.notify(NoticeKind::Info, lang.legacy_apps_added(&added));
            }
            app.mark_dirty();
        }
        if let Some(run) = app.state.unseen_automatic().cloned() {
            let (ok, text) = app.lang.automatic_result(&run);
            app.notify(
                if ok {
                    NoticeKind::Success
                } else {
                    NoticeKind::Warning
                },
                text,
            );
            app.state = State::update(&app.paths.config_file, |state| {
                state.acknowledged_at = Some(chrono::Utc::now());
            });
        }
        app.history = crate::history::load(&app.paths.config_file);
        app.migrate_destination();
        app.refresh_vault();
        app.refresh_sizes(ctx);
        app.refresh_snapshots(ctx);
        app
    }

    fn config_notice(&mut self, notice: ConfigNotice) {
        let (kind, text) = match notice {
            ConfigNotice::Created => (NoticeKind::Info, self.lang.t().config_created.to_string()),
            ConfigNotice::Invalid { kept_copy, message } => (
                NoticeKind::Warning,
                self.lang
                    .config_invalid(&message, &kept_copy.display().to_string()),
            ),
            ConfigNotice::NotSaved { message } => {
                (NoticeKind::Warning, self.lang.config_not_saved(&message))
            }
        };
        self.notify(kind, text);
    }

    pub fn notify(&mut self, kind: NoticeKind, text: impl Into<String>) {
        let text = text.into();
        self.notices.retain(|n| n.text != text);
        let until = Instant::now() + Notice::lifetime(&kind);
        self.notices.push(Notice { kind, text, until });
        if self.notices.len() > 3 {
            self.notices.remove(0);
        }
    }

    /// Notes something in the activity history (not while browsing other backups).
    pub fn record(&mut self, event: crate::history::Event) {
        if self.viewer.is_some() {
            return;
        }
        let entry = crate::history::record(&self.paths.config_file, event);
        self.history.push(entry);
    }

    pub fn mark_dirty(&mut self) {
        self.config_dirty = true;
    }

    /// A backup or restore runs, from the window or automatically.
    pub fn is_busy(&self) -> bool {
        self.task.is_some()
            || self
                .background
                .as_ref()
                .is_some_and(|b| b.service.is_running())
    }

    fn save_if_dirty(&mut self) {
        if self.config_dirty {
            self.save_now();
        }
    }

    pub fn save_now(&mut self) {
        self.config_dirty = false;
        if self.viewer.is_some() {
            // Browsing someone's backups never changes this computer's settings.
            return;
        }
        if let Err(err) = self.config.save(&self.paths.config_file) {
            tracing::error!("configuration could not be saved: {err}");
            self.notify(
                NoticeKind::Error,
                self.lang.config_not_saved(&err.to_string()),
            );
        }
        if let Some(background) = &self.background {
            background.service.set_config(&self.config);
        }
    }

    // --- layout ------------------------------------------------------------------

    fn header(&mut self, ui: &mut Ui) {
        let p = *palette(ui);
        let t = self.lang.t();
        let ctx = ui.ctx().clone();

        egui::Panel::top("header")
            .show_separator_line(false)
            .frame(Frame::new().fill(p.background).inner_margin(Margin {
                left: 32,
                right: 28,
                top: 16,
                bottom: 0,
            }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    widgets::logo(ui, 46.0);
                    ui.add_space(8.0);
                    widgets::title(ui, "AeternaVault");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let current = self.config.appearance;
                        let next = next_appearance(current, ui.visuals().dark_mode);
                        let hint = match next {
                            Appearance::Light => t.switch_to_light,
                            Appearance::Black => t.switch_to_black,
                            _ => t.switch_to_dark,
                        };
                        if theme_toggle(ui, ui.visuals().dark_mode)
                            .on_hover_text(hint)
                            .clicked()
                        {
                            self.config.appearance = next;
                            theme::apply(ui.ctx(), self.config.appearance);
                            self.mark_dirty();
                        }
                        if widgets::button(ui, ButtonKind::Quiet, t.switch_language, true)
                            .on_hover_text(t.switch_language_hint)
                            .clicked()
                        {
                            self.config.language = match self.lang {
                                Lang::En => crate::config::LanguageSetting::De,
                                Lang::De => crate::config::LanguageSetting::En,
                            };
                            self.lang = Lang::resolve(self.config.language);
                            self.mark_dirty();
                        }
                    });
                });

                ui.add_space(12.0);
                // Wraps into a second row when the window is narrow.
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(20.0, 2.0);
                    for (number, view) in self.visible_views().into_iter().enumerate() {
                        let selected = self.view == view && matches!(self.screen, Screen::Main);
                        let label = view_label(t, view);
                        let update_ready =
                            view == View::Settings && self.updater.available().is_some();
                        let tab = widgets::nav_tab(ui, label, selected, true);
                        // A small dot instead of a pop-up when a new version is out.
                        if update_ready {
                            ui.painter().circle_filled(
                                egui::pos2(tab.rect.right() + 6.0, tab.rect.top() + 7.0),
                                3.5,
                                p.accent,
                            );
                        }
                        let hint = if update_ready {
                            format!("Ctrl+{} · {}", number + 1, t.update_tab_hint)
                        } else {
                            format!("Ctrl+{}", number + 1)
                        };
                        if tab.on_hover_text(hint).clicked() {
                            self.navigate(&ctx, view);
                        }
                    }
                });
                let rect = ui.max_rect();
                ui.painter().line_segment(
                    [
                        egui::pos2(rect.left() - 32.0, ui.cursor().top()),
                        egui::pos2(rect.right() + 28.0, ui.cursor().top()),
                    ],
                    Stroke::new(1.0, p.border),
                );
            });
    }

    /// While a task runs and another tab is shown: its progress, and a way back.
    fn task_strip(&mut self, ui: &mut Ui) {
        if matches!(self.screen, Screen::Working) {
            return;
        }
        let Some(task) = &self.task else {
            return;
        };
        let p = *palette(ui);
        let t = self.lang.t();
        let title = views::working::task_title(self.lang, task.kind);
        let fraction = task.progress.fraction();
        let mut show = false;
        let mut cancel = false;
        egui::Panel::top("task-strip")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .inner_margin(Margin::symmetric(32, 8)),
            )
            .show(ui, |ui| {
                widgets::centered_column(ui, COLUMN, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(16.0));
                        widgets::strong_text(
                            ui,
                            match fraction {
                                Some(f) => format!("{title} · {:.0} %", f * 100.0),
                                None => title.to_string(),
                            },
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::button(ui, ButtonKind::Quiet, t.cancel, true).clicked() {
                                cancel = true;
                            }
                            if widgets::button(ui, ButtonKind::Secondary, t.show_progress, true)
                                .clicked()
                            {
                                show = true;
                            }
                        });
                    });
                    widgets::progress_line(ui, fraction);
                });
            });
        if show {
            self.screen = Screen::Working;
        }
        if cancel && let Some(task) = &self.task {
            task.cancel.cancel();
        }
    }

    /// Notices get their own strip at the very top of the window, so they never
    /// cover anything, and disappear after a few seconds.
    fn notices_panel(&mut self, ui: &mut Ui) {
        let now = Instant::now();
        self.notices.retain(|n| n.until > now);
        if self.notices.is_empty() {
            return;
        }
        let p = *palette(ui);
        let mut dismissed = None;
        let mut hovered = Vec::new();
        egui::Panel::top("notices")
            .show_separator_line(false)
            .frame(Frame::new().fill(p.background).inner_margin(Margin {
                left: 32,
                right: 28,
                top: 10,
                bottom: 0,
            }))
            .show(ui, |ui| {
                widgets::centered_column(ui, COLUMN, |ui| {
                    for (i, n) in self.notices.iter().enumerate() {
                        let (closed, response) = widgets::notice(ui, &n.kind, &n.text);
                        if closed {
                            dismissed = Some(i);
                        }
                        if response.contains_pointer() {
                            hovered.push(i);
                        }
                        ui.add_space(4.0);
                    }
                });
            });
        for i in hovered {
            let hold = now + std::time::Duration::from_secs(3);
            if let Some(n) = self.notices.get_mut(i) {
                n.until = n.until.max(hold);
            }
        }
        if let Some(i) = dismissed {
            self.notices.remove(i);
        }
        if let Some(next) = self.notices.iter().map(|n| n.until).min() {
            ui.ctx()
                .request_repaint_after(next.saturating_duration_since(now));
        }
    }

    /// In the viewer (opened by double-click), a line that says what is shown
    /// and how to leave.
    fn viewer_bar(&mut self, ui: &mut Ui) {
        let Some(folder) = self.viewer.clone() else {
            return;
        };
        let p = *palette(ui);
        let t = self.lang.t();
        egui::Panel::top("viewer")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.panel)
                    .inner_margin(Margin::symmetric(32, 8)),
            )
            .show(ui, |ui| {
                widgets::centered_column(ui, COLUMN, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    self.lang.viewer_banner(&folder.display().to_string()),
                                )
                                .color(p.text_secondary),
                            )
                            .truncate(),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::button(ui, ButtonKind::Secondary, t.close_viewer, true)
                                .clicked()
                            {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        });
                    });
                });
            });
    }

    fn footer(&self, ui: &mut Ui) {
        let p = *palette(ui);
        egui::Panel::bottom("footer")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(p.background)
                    .inner_margin(Margin::symmetric(32, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    widgets::secondary_text(
                        ui,
                        format!("AeternaVault {}", env!("CARGO_PKG_VERSION")),
                    );
                    if self.paths.portable {
                        widgets::secondary_text(ui, "·");
                        widgets::secondary_text(ui, self.lang.t().portable_mode);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        widgets::secondary_text(ui, &self.computer);
                    });
                });
            });
    }

    fn handle_dropped_folders(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        if dropped.is_empty() || self.viewer.is_some() {
            return;
        }
        for path in dropped.into_iter().filter(|p| p.is_dir()) {
            self.add_folder(ctx, path);
        }
        self.navigate(ctx, View::Backup);
    }
}

pub fn view_label(t: &crate::i18n::Tr, view: View) -> &'static str {
    match view {
        View::Backup => t.nav_backup,
        View::Restore => t.nav_restore,
        View::Backups => t.nav_backups,
        View::Jobs => t.nav_jobs,
        View::Apps => t.nav_apps,
        View::Notifications => t.nav_notifications,
        View::Settings => t.nav_settings,
        View::Activity => t.nav_activity,
    }
}

/// The appearance switch in the header cycles light → dark → black.
fn next_appearance(current: Appearance, dark_now: bool) -> Appearance {
    match current {
        Appearance::Light => Appearance::Dark,
        Appearance::Dark => Appearance::Black,
        Appearance::Black => Appearance::Light,
        Appearance::System if dark_now => Appearance::Black,
        Appearance::System => Appearance::Dark,
    }
}

/// Appearance switch drawn as a half-filled circle (a common, wordless symbol).
fn theme_toggle(ui: &mut Ui, dark: bool) -> egui::Response {
    let p = *palette(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(34.0, 32.0), egui::Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), p.raised);
    }
    let color = if hovered { p.text } else { p.text_secondary };
    let c = rect.center();
    let r = 8.0;
    ui.painter().circle_stroke(c, r, Stroke::new(1.3, color));
    let half: Vec<egui::Pos2> = (0..=24)
        .map(|i| {
            let a = std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 24.0;
            let a = if dark { a + std::f32::consts::PI } else { a };
            c + egui::vec2(a.cos(), a.sin()) * r
        })
        .collect();
    ui.painter()
        .add(egui::Shape::convex_polygon(half, color, Stroke::NONE));
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Appearance"));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

impl eframe::App for AeternaApp {
    /// Runs before every frame, and also while the window is hidden.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // The new version is being installed and takes over from here.
        if self.updater.wants_exit() {
            self.quit(ctx);
            return;
        }
        self.poll_jobs(ctx);
        self.poll_manage_job(ctx);
        self.background_tick(ctx);
        self.expire_read_access(ctx);
        self.save_if_dirty();
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.autoscroll_hook(raw_input);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_dropped_folders(&ctx);
        self.handle_shortcuts(&ctx);

        self.notices_panel(ui);
        self.header(ui);
        self.viewer_bar(ui);
        self.task_strip(ui);
        self.footer(ui);

        let p = *palette(ui);
        if matches!(self.screen, Screen::Main) && matches!(self.view, View::Backup | View::Restore)
        {
            egui::Panel::bottom("actions")
                .show_separator_line(false)
                .frame(
                    Frame::new()
                        .fill(p.panel)
                        .inner_margin(Margin::symmetric(32, 14)),
                )
                .show(ui, |ui| {
                    widgets::centered_column(ui, COLUMN, |ui| match self.view {
                        View::Restore => views::restore::action_bar(self, ui),
                        _ => views::backup::action_bar(self, ui),
                    });
                });
        }

        egui::CentralPanel::default()
            .frame(Frame::new().fill(p.background).inner_margin(Margin {
                left: 32,
                right: 32,
                top: 20,
                bottom: 12,
            }))
            .show(ui, |ui| match &self.screen {
                Screen::Browse(_) => views::browse::show(self, ui),
                Screen::Working => views::working::show_working(self, ui),
                Screen::Done(_) => views::working::show_done(self, ui),
                Screen::Main => {
                    egui::ScrollArea::vertical()
                        .id_salt(("main", self.view as u8))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            widgets::centered_column(ui, COLUMN, |ui| match self.view {
                                View::Backup => views::backup::show(self, ui),
                                View::Restore => views::restore::show(self, ui),
                                View::Backups => views::backups::show(self, ui),
                                View::Jobs => views::jobs::show(self, ui),
                                View::Apps => views::apps::show(self, ui),
                                View::Notifications => views::notifications::show(self, ui),
                                View::Settings => views::settings::show(self, ui),
                                View::Activity => views::activity::show(self, ui),
                            });
                        });
                }
            });

        dialogs::confirmation(self, ui);
        dialogs::vault_dialog(self, ui);
        dialogs::schedule_dialog(self, ui);
        self.paint_autoscroll(&ctx);
        self.save_if_dirty();
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        theme::palette_for(visuals.dark_mode)
            .background
            .to_normalized_gamma_f32()
    }
}
