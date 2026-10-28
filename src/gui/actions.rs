//! What happens when the user clicks: background loaders, backup and restore
//! tasks, the encrypted vault and the automatic-backup schedule.

use std::path::{Path, PathBuf};
use std::time::Instant;

use eframe::egui;

use super::tasks::{Job, Task, TaskKind, TaskOutput};
use super::widgets::NoticeKind;
use super::{
    AeternaApp, AfterUnlock, DetectedApp, Done, Pending, Screen, TreeEntry, VaultDialog, View,
};
use crate::config::Schedule;
use crate::config::Source;
use crate::engine::layout;
use crate::engine::plan::{
    self, BackupInput, BackupPlan, ItemKind, RestoreOptions, RestorePlan, RestoreTarget,
};
use crate::engine::restore;
use crate::engine::selection::Selection;
use crate::engine::snapshots::{self, SnapshotInfo};
use crate::engine::sources::Sources;
use crate::engine::{backup, paths_equal, scan, vault};
use crate::error::EngineError;
use crate::i18n::Lang;
use crate::platform::known_paths::KnownPaths;
use crate::platform::{self, vss::LiveFiles};
use crate::state;

impl AeternaApp {
    fn key_dir(&self) -> PathBuf {
        state::key_dir(&self.paths.config_file)
    }

    // --- sources and sizes ---------------------------------------------------------

    pub fn add_source(&mut self, ctx: &egui::Context, source: Source) {
        if self.config.has_source_path(&source.path) {
            return;
        }
        tracing::info!("source added: {}", source.path.display());
        self.config.sources.push(source);
        self.mark_dirty();
        self.refresh_sizes(ctx);
    }

    pub fn add_folder(&mut self, ctx: &egui::Context, path: PathBuf) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.add_source(ctx, Source::new(name, path, true));
    }

    pub fn size_of(&self, path: &Path) -> Option<&Option<(u64, u64)>> {
        self.sizes
            .iter()
            .find(|(p, _)| paths_equal(p, path))
            .map(|(_, size)| size)
    }

    /// Forget the size of a source after its selection changed.
    pub fn invalidate_size(&mut self, ctx: &egui::Context, path: &Path) {
        self.sizes.retain(|p, _| !paths_equal(p, path));
        self.refresh_sizes(ctx);
    }

    pub fn refresh_sizes(&mut self, ctx: &egui::Context) {
        if let Some(job) = self.sizes_job.take() {
            job.cancel.cancel();
        }
        let pending: Vec<(PathBuf, Vec<String>, Vec<String>)> = self
            .config
            .sources
            .iter()
            .filter(|s| self.size_of(&s.path).and_then(|s| s.as_ref()).is_none())
            .map(|s| {
                (
                    s.path.clone(),
                    s.include_paths.clone(),
                    s.exclude_paths.clone(),
                )
            })
            .collect();
        if pending.is_empty() {
            return;
        }
        self.sizes_job = Some(Job::spawn(ctx, move |cancel, send| {
            for (path, include, exclude) in pending {
                if cancel.is_cancelled() {
                    return;
                }
                let size = scan::folder_size(&path, &Selection::new(&include, &exclude), cancel);
                send((path, size));
            }
        }));
    }

    /// Entries of a folder for the "choose contents" tree (loaded once, then cached).
    pub fn tree_listing(&mut self, dir: &Path) -> Vec<TreeEntry> {
        if let Some(entries) = self.tree.listings.get(dir) {
            return entries.clone();
        }
        let mut entries: Vec<TreeEntry> = std::fs::read_dir(dir)
            .map(|read| {
                read.flatten()
                    .filter_map(|entry| {
                        let file_type = entry.file_type().ok()?;
                        if file_type.is_symlink() {
                            return None;
                        }
                        Some(TreeEntry {
                            name: entry.file_name().to_string_lossy().into_owned(),
                            is_dir: file_type.is_dir(),
                            size: if file_type.is_file() {
                                entry.metadata().map(|m| m.len()).unwrap_or(0)
                            } else {
                                0
                            },
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.tree
            .listings
            .insert(dir.to_path_buf(), entries.clone());
        entries
    }

    // --- applications ----------------------------------------------------------------

    /// Looks for the applications of the catalog on this computer (and the
    /// size of their folders, which can take a moment).
    pub fn refresh_apps(&mut self, ctx: &egui::Context) {
        if let Some(job) = self.apps.job.take() {
            job.cancel.cancel();
        }
        let catalog = self.catalog.clone();
        self.apps.job = Some(Job::spawn(ctx, move |cancel, send| {
            let known = KnownPaths::current();
            let mut found: Vec<DetectedApp> = catalog
                .detected(&known)
                .into_iter()
                .map(|(app, folders)| DetectedApp {
                    id: app.id.clone(),
                    category: app.category,
                    folders,
                    bytes: None,
                })
                .collect();
            // First the list, so it appears immediately …
            send(found.clone());
            // … then the sizes.
            for index in 0..found.len() {
                if cancel.is_cancelled() {
                    return;
                }
                let mut total = 0;
                for folder in found[index].folders.clone() {
                    let everything = Selection::new(&[], &[]);
                    if let Some((bytes, _)) = scan::folder_size(&folder, &everything, cancel) {
                        total += bytes;
                    }
                }
                found[index].bytes = Some(total);
                send(found.clone());
            }
        }));
    }

    /// Whether all folders of a detected application are in the list.
    pub fn app_added(&self, app: &DetectedApp) -> bool {
        app.folders.iter().all(|f| self.config.has_source_path(f))
    }

    /// Adds the folders of an application to the folders to back up and shows
    /// them, opened, in the Backup view.
    pub fn add_app(&mut self, ctx: &egui::Context, detected: &DetectedApp) {
        let Some(app) = self.catalog.get(&detected.id).cloned() else {
            return;
        };
        let name = app.display_name(self.lang).to_string();
        let mut first = None;
        for folder in &detected.folders {
            if self.config.has_source_path(folder) {
                continue;
            }
            let mut source = Source::new(name.clone(), folder.clone(), true);
            source.exclude = app.exclude.clone();
            if self.config.encryption.selected() && app.sensitive {
                source.encrypt_paths = vec![crate::engine::selection::ROOT.to_string()];
            }
            self.add_source(ctx, source);
            self.tree.expanded_sources.insert(folder.clone());
            first.get_or_insert(folder.clone());
        }
        if let Some(folder) = first {
            self.tree.reveal = Some(folder);
            self.notify(NoticeKind::Success, self.lang.folder_added(&name, false));
            if app.sensitive && !self.config.encryption.enabled {
                self.notify(NoticeKind::Info, self.lang.t().sensitive_app_hint);
            }
            self.navigate(ctx, View::Backup);
        }
    }

    /// Removes an application's folders from the list (nothing is deleted).
    pub fn remove_app(&mut self, detected: &DetectedApp) {
        self.config
            .sources
            .retain(|s| !detected.folders.iter().any(|f| paths_equal(f, &s.path)));
        self.mark_dirty();
    }

    pub fn collect_sources(&self) -> Sources {
        Sources::collect(&self.config, &KnownPaths::current())
    }

    // --- snapshots ----------------------------------------------------------------------

    pub fn refresh_snapshots(&mut self, ctx: &egui::Context) {
        let destination = self.config.destination.clone();
        if destination.as_os_str().is_empty() {
            self.snapshots = Some(Err(EngineError::NoDestination));
            return;
        }
        let key = self.vault.key.clone();
        self.snapshots_job = Some(Job::spawn(ctx, move |_, send| {
            let list = snapshots::list(&destination, key.as_ref());
            let free = platform::free_space(&destination);
            send((list, free));
        }));
    }

    pub fn last_backup(&self) -> Option<&SnapshotInfo> {
        match &self.snapshots {
            Some(Ok(list)) => list
                .iter()
                .find(|s| s.computer.eq_ignore_ascii_case(&self.computer) || s.is_locked()),
            _ => None,
        }
    }

    pub fn destination_reachable(&self) -> Option<bool> {
        match &self.snapshots {
            Some(Err(EngineError::DestinationUnavailable(_))) => Some(false),
            Some(_) => Some(true),
            None => None,
        }
    }

    /// Whether the destination is on the same drive as a ticked folder: such
    /// a backup does not survive a failing drive.
    pub fn destination_shares_drive(&self) -> bool {
        let destination = &self.config.destination;
        !destination.as_os_str().is_empty()
            && self
                .config
                .enabled_sources()
                .any(|s| crate::platform::same_drive(destination, &s.path))
    }

    pub fn selected_snapshot(&self) -> Option<&SnapshotInfo> {
        let selected = self.restore.selected.as_ref()?;
        match &self.snapshots {
            Some(Ok(list)) => list.iter().find(|s| &s.qualified_id() == selected),
            _ => None,
        }
    }

    /// Lets the user pick a folder; with the option on, backups go into an
    /// `AeternaVault` folder inside it.
    pub fn choose_destination(&mut self, ctx: &egui::Context) {
        let Some(folder) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        self.config.destination =
            snapshots::chosen_destination(&folder, self.config.advanced.destination_app_folder);
        tracing::info!("destination chosen: {}", self.config.destination.display());
        self.record(crate::history::Event::DestinationChanged {
            destination: self.config.destination.display().to_string(),
        });
        self.mark_dirty();
        self.destination_changed(ctx);
    }

    /// Switching the option also moves the current destination in or out of
    /// its `AeternaVault` folder, unless backups already live there.
    pub fn set_destination_app_folder(&mut self, ctx: &egui::Context, on: bool) {
        self.config.advanced.destination_app_folder = on;
        self.mark_dirty();
        let current = self.config.destination.clone();
        if current.as_os_str().is_empty() || snapshots::contains_backups(&current) {
            return;
        }
        let named = current.file_name().is_some_and(|n| {
            n.to_string_lossy()
                .eq_ignore_ascii_case(snapshots::APP_FOLDER)
        });
        let adjusted = match (on, named, current.parent()) {
            (true, false, _) => Some(current.join(snapshots::APP_FOLDER)),
            (false, true, Some(parent)) if !parent.as_os_str().is_empty() => {
                Some(parent.to_path_buf())
            }
            _ => None,
        };
        if let Some(destination) = adjusted {
            self.record(crate::history::Event::DestinationChanged {
                destination: destination.display().to_string(),
            });
            self.config.destination = destination;
            self.destination_changed(ctx);
        }
    }

    pub fn set_explorer_menu(&mut self, on: bool) {
        self.config.advanced.explorer_menu = on;
        self.mark_dirty();
        if !platform::system_changes_allowed() {
            return;
        }
        if let Err(err) =
            platform::context_menu::set_registered(on, self.lang.t().explorer_menu_label)
        {
            self.notify(NoticeKind::Error, err.to_string());
        }
    }

    pub fn set_open_by_double_click(&mut self, on: bool) {
        self.config.advanced.open_by_double_click = on;
        self.mark_dirty();
        if !platform::system_changes_allowed() {
            return;
        }
        if let Err(err) = platform::file_association::set_registered(on) {
            self.notify(NoticeKind::Error, err.to_string());
        }
    }

    pub fn destination_changed(&mut self, ctx: &egui::Context) {
        self.vault.key = None;
        self.refresh_vault();
        self.refresh_snapshots(ctx);
    }

    pub(super) fn poll_jobs(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.sizes_job {
            let (values, finished) = job.drain();
            for (path, size) in values {
                self.sizes.insert(path, size);
            }
            if finished {
                self.sizes_job = None;
            }
        }

        if let Some(job) = &self.snapshots_job {
            let (mut values, finished) = job.drain();
            if let Some((list, free)) = values.pop() {
                if let Ok(list) = &list {
                    let still_there = self
                        .restore
                        .selected
                        .as_ref()
                        .is_some_and(|sel| list.iter().any(|s| &s.qualified_id() == sel));
                    if !still_there {
                        self.restore.skip.clear();
                        self.restore.selected = list
                            .iter()
                            .find(|s| {
                                s.header.is_some()
                                    && s.computer.eq_ignore_ascii_case(&self.computer)
                            })
                            .or_else(|| list.first())
                            .map(|s| s.qualified_id());
                    }
                }
                self.snapshots = Some(list);
                self.destination_free = free;
            }
            if finished {
                self.snapshots_job = None;
            }
        }

        if let Some(job) = &self.apps.job {
            let (mut values, finished) = job.drain();
            if let Some(list) = values.pop() {
                self.apps.detected = Some(list);
            }
            if finished {
                self.apps.job = None;
            }
        }

        if let Some(task) = &mut self.task
            && let Some(result) = task.poll()
        {
            let kind = task.kind;
            self.task = None;
            self.finish_task(ctx, kind, result);
        }
    }

    // --- backup and restore ---------------------------------------------------------------

    /// Checks the destination and the folders, then asks for confirmation (if
    /// set) and backs up.
    pub fn plan_backup(&mut self, ctx: &egui::Context) {
        if self.is_busy() {
            return;
        }
        let sources = self.collect_sources();
        // Check the destination again before every backup: the folder or the
        // encrypted vault in it may have been deleted or moved in the meantime.
        self.migrate_destination();
        self.refresh_vault();
        if sources.may_encrypt() {
            if self.vault.header.is_none() {
                self.notify(NoticeKind::Warning, self.lang.t().vault_missing);
                self.vault.dialog = Some(VaultDialog::Create {
                    passphrase: String::new(),
                    repeat: String::new(),
                    remember: true,
                    options: self.config.encryption.vault_options(),
                    error: None,
                });
                return;
            }
            if self.vault.key.is_none() {
                self.open_unlock(AfterUnlock::Backup);
                return;
            }
        }
        let config = self.config.clone();
        let computer = self.computer.clone();
        let key = self.vault.key.clone();
        self.task = Some(Task::spawn(
            ctx,
            TaskKind::PlanBackup,
            move |cancel, progress| {
                let input = BackupInput {
                    config: &config,
                    sources: &sources,
                    computer: &computer,
                    key: key.as_ref(),
                };
                TaskOutput::BackupPlan(plan::plan_backup(&input, cancel, progress))
            },
        ));
        self.screen = Screen::Working;
    }

    pub fn plan_restore(&mut self, ctx: &egui::Context) {
        if self.is_busy() {
            return;
        }
        let Some(snapshot) = self.selected_snapshot().cloned() else {
            return;
        };
        if !self.ensure_read_access(&snapshot) {
            return;
        }
        let target = if self.restore.to_folder {
            match &self.restore.folder {
                Some(folder) => RestoreTarget::Folder(folder.clone()),
                None => {
                    self.notify(NoticeKind::Warning, self.lang.t().choose_folder_first);
                    return;
                }
            }
        } else {
            RestoreTarget::Original
        };
        let options = RestoreOptions {
            target,
            conflict: self.config.advanced.restore_conflict,
            verify: self.config.advanced.verify_on_restore,
            skip: self.restore.skip.clone(),
        };
        let key = self.vault.key.clone();
        self.task = Some(Task::spawn(
            ctx,
            TaskKind::PlanRestore,
            move |cancel, progress| {
                TaskOutput::RestorePlan(plan::plan_restore(
                    &snapshot,
                    options,
                    key.as_ref(),
                    cancel,
                    progress,
                ))
            },
        ));
        self.screen = Screen::Working;
    }

    pub fn start_backup(&mut self, ctx: &egui::Context, plan: Box<BackupPlan>) {
        if self.is_busy() {
            return;
        }
        let key = self.vault.key.clone();
        self.task = Some(Task::spawn(
            ctx,
            TaskKind::Backup,
            move |cancel, progress| {
                TaskOutput::Backup(backup::run_backup(
                    &plan,
                    key.as_ref(),
                    &LiveFiles,
                    cancel,
                    progress,
                ))
            },
        ));
        self.screen = Screen::Working;
    }

    pub fn start_restore(&mut self, ctx: &egui::Context, plan: Box<RestorePlan>) {
        if self.is_busy() {
            return;
        }
        let key = self.vault.key.clone();
        self.task = Some(Task::spawn(
            ctx,
            TaskKind::Restore,
            move |cancel, progress| {
                TaskOutput::Restore(restore::run_restore(&plan, key.as_ref(), cancel, progress))
            },
        ));
        self.screen = Screen::Working;
    }

    fn finish_task(
        &mut self,
        ctx: &egui::Context,
        kind: TaskKind,
        result: Result<TaskOutput, String>,
    ) {
        let output = match result {
            Ok(output) => output,
            Err(panic) => {
                tracing::error!("background task failed: {panic}");
                self.notify(NoticeKind::Error, self.lang.t().unexpected_problem);
                self.screen = Screen::Main;
                return;
            }
        };

        match output {
            TaskOutput::BackupPlan(Ok(plan)) => {
                if self.config.advanced.confirm_before_start {
                    self.pending = Some(Pending::Backup(Box::new(plan)));
                    self.leave_working();
                } else {
                    self.start_backup(ctx, Box::new(plan));
                }
            }
            TaskOutput::RestorePlan(Ok(plan)) => {
                // Overwriting files always asks, regardless of the setting.
                let replaces = plan.summary.count(ItemKind::Changed) > 0;
                if self.config.advanced.confirm_before_start || replaces {
                    self.pending = Some(Pending::Restore(Box::new(plan)));
                    self.leave_working();
                } else {
                    self.start_restore(ctx, Box::new(plan));
                }
            }
            TaskOutput::BackupPlan(Err(err)) | TaskOutput::RestorePlan(Err(err)) => {
                self.leave_working();
                match err {
                    EngineError::Cancelled => {}
                    EngineError::Locked => self.open_unlock(AfterUnlock::Read),
                    err => {
                        tracing::warn!("planning failed: {err}");
                        self.notify(NoticeKind::Warning, self.lang.error_message(&err));
                    }
                }
            }
            TaskOutput::Backup(result) => {
                self.record(crate::history::backup_event(&result, false));
                crate::notify::after_manual_backup(
                    &self.config,
                    &result,
                    self.lang.t().back_up_now,
                );
                let result = result.map_err(|e| {
                    tracing::error!("backup failed: {e}");
                    self.lang.error_message(&e)
                });
                if result.is_ok() {
                    self.apply_retention_quietly(ctx);
                }
                self.present_done(Done::Backup(result));
                self.refresh_snapshots(ctx);
            }
            TaskOutput::Restore(result) => {
                let snapshot = self
                    .selected_snapshot()
                    .map(|s| s.id.clone())
                    .unwrap_or_default();
                let target = match (self.restore.to_folder, &self.restore.folder) {
                    (true, Some(folder)) => folder.display().to_string(),
                    _ => String::new(),
                };
                self.record(crate::history::restore_event(&result, &snapshot, &target));
                let result = result.map_err(|e| {
                    tracing::error!("restore failed: {e}");
                    self.lang.error_message(&e)
                });
                self.present_done(Done::Restore(result));
                self.sizes.clear();
                self.tree.listings.clear();
                self.refresh_sizes(ctx);
            }
            other => self.finish_manage_task(ctx, kind, other),
        }
    }

    pub fn reload_config(&mut self, ctx: &egui::Context) {
        let loaded = crate::config::load_or_create(&self.paths);
        self.config = loaded.config;
        self.lang = Lang::resolve(self.config.language);
        self.settings.exclude_text = self.config.exclude.join("\n");
        let config_dir = self
            .paths
            .config_file
            .parent()
            .map(PathBuf::from)
            .unwrap_or_default();
        self.catalog = platform::apps::Catalog::load(&config_dir);
        super::theme::install_fonts(ctx, &self.config.fonts);
        super::theme::apply(ctx, self.config.appearance);
        ctx.set_zoom_factor(self.config.zoom_factor());
        self.sizes.clear();
        self.tree.listings.clear();
        self.destination_changed(ctx);
        self.refresh_sizes(ctx);
        self.refresh_apps(ctx);
        match loaded.notice {
            Some(notice) => self.config_notice(notice),
            None => self.notify(NoticeKind::Success, self.lang.t().config_reloaded),
        }
    }

    // --- encrypted vault ---------------------------------------------------------------

    /// Reads the vault header at the destination and a remembered key, if any.
    pub fn refresh_vault(&mut self) {
        let header = vault::read_header(&self.config.destination).ok();
        let same_vault = match (&self.vault.header, &header) {
            (Some(a), Some(b)) => a.vault_id == b.vault_id,
            _ => false,
        };
        if !same_vault {
            self.vault.key = None;
        }
        if let Some(header) = &header {
            if self.vault.key.is_none() {
                self.vault.key = vault::remembered_key(&self.key_dir(), header);
            }
            self.vault.remembered = vault::is_remembered(&self.key_dir(), header);
        } else {
            self.vault.remembered = false;
        }
        self.vault.header = header;
    }

    pub fn open_unlock(&mut self, then: AfterUnlock) {
        self.vault.dialog = Some(VaultDialog::Unlock {
            secret: String::new(),
            remember: then == AfterUnlock::Remember || self.config.any_schedule_enabled(),
            error: None,
            then,
        });
    }

    /// Turns encryption on or off from the Backup view.
    pub fn set_encryption(&mut self, enabled: bool) {
        if !enabled {
            self.config.encryption.enabled = false;
            self.record(crate::history::Event::EncryptionSwitched { on: false });
            self.mark_dirty();
            self.notify(NoticeKind::Info, self.lang.t().encryption_disabled_note);
            return;
        }
        self.refresh_vault();
        match (&self.vault.header, &self.vault.key) {
            (Some(_), Some(_)) => {
                self.config.encryption.enabled = true;
                self.record(crate::history::Event::EncryptionSwitched { on: true });
                self.mark_dirty();
            }
            (Some(_), None) => self.open_unlock(AfterUnlock::EnableEncryption),
            (None, _) => {
                self.vault.dialog = Some(VaultDialog::Create {
                    passphrase: String::new(),
                    repeat: String::new(),
                    remember: true,
                    options: self.config.encryption.vault_options(),
                    error: None,
                });
            }
        }
    }

    /// Validates a new passphrase; returns a message for the user if unsuitable.
    pub fn passphrase_problem(&self, passphrase: &str, repeat: &str) -> Option<String> {
        let t = self.lang.t();
        // Only an empty passphrase is refused; how good it is, is the user's choice.
        if passphrase.is_empty() {
            Some(t.passphrase_empty.to_string())
        } else if passphrase != repeat {
            Some(t.passphrases_differ.to_string())
        } else {
            None
        }
    }

    /// Creates the vault; returns the recovery key to show.
    pub fn create_vault(
        &mut self,
        passphrase: &str,
        remember: bool,
        options: vault::VaultOptions,
    ) -> Result<String, String> {
        let destination = self.config.destination.clone();
        if destination.as_os_str().is_empty() {
            return Err(self.lang.error_message(&EngineError::NoDestination));
        }
        let created = vault::create(&destination, passphrase, options)
            .map_err(|e| self.lang.error_message(&e.into()))?;
        if remember
            && let Err(err) = vault::remember_key(&self.key_dir(), &created.header, &created.key)
        {
            tracing::warn!("vault key could not be remembered: {err}");
        }
        tracing::info!("encrypted vault created at {}", destination.display());
        self.record(crate::history::Event::EncryptionSetUp {
            cipher: options.cipher.display_name().to_string(),
        });
        self.vault.header = Some(created.header);
        self.vault.key = Some(created.key);
        self.refresh_vault();
        self.config.encryption.enabled = true;
        self.mark_dirty();
        Ok(created.recovery_key)
    }

    pub fn unlock_vault(
        &mut self,
        ctx: &egui::Context,
        secret: &str,
        remember: bool,
        then: AfterUnlock,
    ) -> Result<(), String> {
        let (header, key) = vault::unlock(&self.config.destination, secret).map_err(|e| {
            if matches!(e, crate::engine::crypto::CryptoError::WrongKey) {
                self.lang.t().wrong_passphrase.to_string()
            } else {
                self.lang.error_message(&e.into())
            }
        })?;
        if remember && let Err(err) = vault::remember_key(&self.key_dir(), &header, &key) {
            tracing::warn!("vault key could not be remembered: {err}");
        }
        self.vault.header = Some(header);
        self.vault.key = Some(key);
        self.refresh_vault();
        self.refresh_snapshots(ctx);

        // Typing the passphrase (or recovery key) allows reading for a while.
        self.vault.read_until = Some(Instant::now() + super::READ_ACCESS);
        match then {
            AfterUnlock::Backup => self.plan_backup(ctx),
            AfterUnlock::EnableEncryption => {
                self.config.encryption.enabled = true;
                self.record(crate::history::Event::EncryptionSwitched { on: true });
                self.mark_dirty();
            }
            AfterUnlock::ChangePassphrase => {
                self.vault.dialog = Some(VaultDialog::Change {
                    passphrase: String::new(),
                    repeat: String::new(),
                    error: None,
                });
            }
            AfterUnlock::ReplaceRecovery => self.replace_recovery_key(),
            AfterUnlock::Read | AfterUnlock::Remember => {}
        }
        Ok(())
    }

    pub fn change_passphrase(&mut self, passphrase: &str) -> Result<(), String> {
        let Some(key) = &self.vault.key else {
            return Err(self.lang.error_message(&EngineError::Locked));
        };
        vault::change_passphrase(&self.config.destination, key, passphrase)
            .map_err(|e| self.lang.error_message(&e.into()))?;
        self.refresh_vault();
        tracing::info!("vault passphrase changed");
        self.record(crate::history::Event::PassphraseChanged);
        Ok(())
    }

    /// Checks a recovery key without changing anything.
    pub fn test_recovery_key(&self, secret: &str) -> Result<(), String> {
        let t = self.lang.t();
        match vault::check_secret(&self.config.destination, secret) {
            Ok(crate::engine::crypto::SlotKind::RecoveryKey) => Ok(()),
            Ok(crate::engine::crypto::SlotKind::Passphrase) => {
                Err(t.test_recovery_is_passphrase.to_string())
            }
            Err(crate::engine::crypto::CryptoError::WrongKey) => {
                Err(t.test_recovery_wrong.to_string())
            }
            Err(err) => Err(self.lang.error_message(&err.into())),
        }
    }

    /// Creates a new recovery key (the old one stops working) and shows it.
    pub fn replace_recovery_key(&mut self) {
        let Some(key) = self.vault.key.clone() else {
            self.open_unlock(AfterUnlock::ReplaceRecovery);
            return;
        };
        match vault::replace_recovery_key(&self.config.destination, &key) {
            Ok(recovery) => {
                tracing::info!("recovery key replaced");
                self.record(crate::history::Event::RecoveryKeyReplaced);
                self.refresh_vault();
                self.vault.dialog = Some(VaultDialog::ShowRecovery {
                    key: recovery,
                    confirmed: false,
                    copied_at: None,
                });
            }
            Err(err) => self.notify(NoticeKind::Error, self.lang.error_message(&err.into())),
        }
    }

    pub fn set_remembered(&mut self, remember: bool) {
        let Some(header) = self.vault.header.clone() else {
            return;
        };
        if remember {
            match &self.vault.key {
                Some(key) => {
                    if let Err(err) = vault::remember_key(&self.key_dir(), &header, key) {
                        self.notify(NoticeKind::Error, err.to_string());
                    }
                }
                None => self.open_unlock(AfterUnlock::Remember),
            }
        } else if let Err(err) = vault::forget_key(&self.key_dir(), &header) {
            self.notify(NoticeKind::Error, err.to_string());
        }
        self.refresh_vault();
    }

    /// Forgets the key in memory (a remembered key is loaded again for backing
    /// up) and ends read access.
    pub fn lock_vault(&mut self, ctx: &egui::Context) {
        self.vault.key = None;
        self.vault.read_until = None;
        self.refresh_vault();
        self.refresh_snapshots(ctx);
        if matches!(&self.screen, Screen::Browse(state) if state.snapshot.needs_key()) {
            self.screen = Screen::Main;
        }
    }

    /// Reading encrypted backups (file names or contents) needs the
    /// passphrase or the recovery key, typed in this session. The remembered
    /// key only serves automatic backups. Asks for it and returns `false` if
    /// it is missing.
    pub fn ensure_read_access(&mut self, snapshot: &SnapshotInfo) -> bool {
        if !snapshot.needs_key() {
            return true;
        }
        if self.vault.can_read() {
            // Reading keeps access alive.
            self.vault.read_until = Some(Instant::now() + super::READ_ACCESS);
            return true;
        }
        self.open_unlock(AfterUnlock::Read);
        false
    }

    /// Ends read access after a while without reading, and when the window is
    /// hidden in the notification area.
    pub(super) fn expire_read_access(&mut self, ctx: &egui::Context) {
        let hidden = self.background.as_ref().is_some_and(|b| b.hidden);
        let expired = self
            .vault
            .read_until
            .is_some_and(|until| until <= Instant::now());
        if self.vault.read_until.is_some() && (expired || hidden) {
            tracing::info!("read access to encrypted backups ended");
            self.vault.read_until = None;
            if matches!(&self.screen, Screen::Browse(state) if state.snapshot.needs_key()) {
                self.screen = Screen::Main;
                self.notify(NoticeKind::Info, self.lang.t().read_access_ended);
            }
        }
        if let Some(until) = self.vault.read_until {
            ctx.request_repaint_after(until.saturating_duration_since(Instant::now()));
        }
    }

    /// Moves backups of AeternaVault 0.3 and 0.4 to the current folder layout.
    pub fn migrate_destination(&mut self) {
        let destination = self.config.destination.clone();
        if destination.as_os_str().is_empty() {
            return;
        }
        match layout::migrate(&destination) {
            Ok(0) => {}
            Ok(moved) => self.notify(NoticeKind::Info, self.lang.layout_migrated(moved)),
            Err(err) => {
                tracing::warn!("older backups could not be moved: {err}");
                self.notify(NoticeKind::Warning, err.to_string());
            }
        }
    }

    /// After planning: back to the page the user was on.
    fn leave_working(&mut self) {
        if matches!(self.screen, Screen::Working) {
            self.screen = Screen::Main;
        }
    }

    // --- automatic backups ---------------------------------------------------------------

    pub fn new_schedule(&mut self) {
        self.schedule.editor = Some(super::ScheduleEditor {
            schedule: Schedule {
                id: Schedule::new_id(),
                ..Schedule::default()
            },
            is_new: true,
        });
    }

    pub fn edit_schedule(&mut self, id: &str) {
        if let Some(schedule) = self.config.schedules.iter().find(|s| s.id == id) {
            self.schedule.editor = Some(super::ScheduleEditor {
                schedule: schedule.clone(),
                is_new: false,
            });
        }
    }

    /// Stores a schedule from the dialog.
    pub fn save_schedule(&mut self, schedule: Schedule) {
        let was_enabled = self
            .config
            .schedules
            .iter()
            .find(|s| s.id == schedule.id)
            .map(|s| s.enabled);
        match self
            .config
            .schedules
            .iter_mut()
            .find(|s| s.id == schedule.id)
        {
            Some(existing) => *existing = schedule.clone(),
            None => self.config.schedules.push(schedule.clone()),
        }
        if schedule.enabled && was_enabled != Some(true) {
            self.arm_schedule(&schedule.id);
        }
        let job = crate::automatic::describe(&schedule, &self.config, self.lang);
        self.record(match was_enabled {
            None => crate::history::Event::JobCreated { job },
            Some(_) => crate::history::Event::JobChanged { job },
        });
        tracing::info!("automatic backup saved: {:?}", schedule.frequency);
        self.mark_dirty();
    }

    pub fn set_schedule_enabled(&mut self, id: &str, enabled: bool) {
        let Some(schedule) = self.config.schedules.iter_mut().find(|s| s.id == id) else {
            return;
        };
        if schedule.enabled == enabled {
            return;
        }
        schedule.enabled = enabled;
        let schedule = schedule.clone();
        let job = crate::automatic::describe(&schedule, &self.config, self.lang);
        if enabled {
            self.arm_schedule(id);
        }
        self.record(crate::history::Event::JobSwitched { job, on: enabled });
        self.mark_dirty();
    }

    pub fn remove_schedule(&mut self, id: &str) {
        if let Some(schedule) = self.config.schedules.iter().find(|s| s.id == id) {
            let job = crate::automatic::describe(schedule, &self.config, self.lang);
            self.record(crate::history::Event::JobRemoved { job });
        }
        self.config.schedules.retain(|s| s.id != id);
        state::State::update(&self.paths.config_file, |state| {
            state.schedules.remove(id);
        });
        self.mark_dirty();
    }

    fn arm_schedule(&mut self, id: &str) {
        match &self.background {
            Some(background) => background.service.arm(id),
            None => {
                let id = id.to_string();
                state::State::update(&self.paths.config_file, |state| {
                    state.schedules.entry(id).or_default().armed_at = Some(chrono::Utc::now());
                });
            }
        }
    }

    pub fn run_schedule_now(&mut self, id: &str) {
        if self.background.is_none() {
            return;
        }
        // The service works with the configuration it was given last.
        self.save_now();
        if let Some(background) = &self.background {
            background.service.run_schedule_now(id);
        }
    }
}
