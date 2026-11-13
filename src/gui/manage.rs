//! Managing backups from the window: check, delete, move, browse, copy out,
//! open single files, and removing old backups by the retention rules.

use std::path::PathBuf;

use eframe::egui;

use super::tasks::{Job, Task, TaskKind, TaskOutput};
use super::widgets::NoticeKind;
use super::{AeternaApp, AfterUnlock, Done, Pending, Screen};
use crate::engine::manage;
use crate::engine::snapshots::{self, SnapshotInfo, StoredFile};
use crate::engine::{retention, verify};
use crate::platform;

/// Opened files are decrypted into this folder and removed at the next start.
/// Folders with copies of opened files: `<temp>/AeternaVault-open-<random>`.
const OPEN_PREFIX: &str = "AeternaVault-open-";

/// Removes copies of opened files from earlier sessions (and the shared
/// folder of AeternaVault 0.8 and older).
pub fn clean_open_folder() {
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("AeternaVault-open"));
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(OPEN_PREFIX))
    {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().unwrap_or_default().as_secs() > 6 * 3600)
            .unwrap_or(true);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

impl AeternaApp {
    /// Shows only the backups at `opened` (a folder or a file inside it).
    pub fn enter_viewer(&mut self, ctx: &egui::Context, opened: &std::path::Path) {
        let destination = crate::engine::vault::destination_for_opened(opened)
            .unwrap_or_else(|| opened.to_path_buf());
        tracing::info!("browsing backups at {}", destination.display());
        self.viewer = Some(destination.clone());
        self.config.destination = destination.clone();
        self.config.schedules.clear();
        self.vault.key = None;
        self.view = super::View::Backups;
        self.refresh_vault();
        self.refresh_snapshots(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "AeternaVault — {}",
            destination.display()
        )));
        if self.vault.header.is_some() && self.vault.key.is_none() {
            self.open_unlock(AfterUnlock::Read);
        }
    }

    pub fn all_snapshots(&self) -> &[SnapshotInfo] {
        match &self.snapshots {
            Some(Ok(list)) => list,
            _ => &[],
        }
    }

    pub fn checked_snapshots(&self) -> Vec<SnapshotInfo> {
        self.all_snapshots()
            .iter()
            .filter(|s| self.manage.checked.contains(&s.qualified_id()))
            .cloned()
            .collect()
    }

    /// Asks for the key first if the backup cannot be handled without it.
    /// (Checking, moving and deleting do not show any content, so the
    /// remembered key is enough.)
    fn ensure_unlocked(&mut self, snapshot: &SnapshotInfo) -> bool {
        if snapshot.needs_unlock() || (snapshot.needs_key() && self.vault.key.is_none()) {
            self.open_unlock(AfterUnlock::Read);
            return false;
        }
        true
    }

    /// Shows the result screen if the user waits for it; otherwise a notice.
    pub(super) fn present_done(&mut self, done: Done) {
        if matches!(self.screen, Screen::Working) {
            self.screen = Screen::Done(Box::new(done));
            return;
        }
        let (ok, title) = super::views::working::done_title(self.lang, &done);
        self.notify(
            if ok {
                NoticeKind::Success
            } else {
                NoticeKind::Warning
            },
            title,
        );
    }

    pub fn verify_snapshot(&mut self, ctx: &egui::Context, snapshot: SnapshotInfo) {
        if self.is_busy() || !self.ensure_unlocked(&snapshot) {
            return;
        }
        let key = self.vault.key.clone();
        // Names of encrypted files only after the passphrase was entered.
        let show_names = self.vault.can_read();
        self.manage.current = Some((snapshot.id.clone(), PathBuf::new()));
        self.task = Some(Task::spawn(
            ctx,
            TaskKind::Verify,
            move |cancel, progress| {
                TaskOutput::Verify(verify::verify(
                    &snapshot,
                    key.as_ref(),
                    show_names,
                    cancel,
                    progress,
                ))
            },
        ));
        self.screen = Screen::Working;
    }

    pub fn ask_delete(&mut self, snapshots: Vec<SnapshotInfo>) {
        if snapshots.is_empty() || self.is_busy() {
            return;
        }
        if snapshots.iter().any(|s| s.needs_key()) && self.vault.key.is_none() {
            self.open_unlock(AfterUnlock::Read);
            return;
        }
        self.pending = Some(Pending::Delete(snapshots));
    }

    pub fn start_delete(&mut self, ctx: &egui::Context, snapshots: Vec<SnapshotInfo>) {
        if self.is_busy() {
            return;
        }
        let destination = self.config.destination.clone();
        let ids: Vec<String> = snapshots.iter().map(SnapshotInfo::qualified_id).collect();
        let key = self.vault.key.clone();
        self.task = Some(Task::spawn(
            ctx,
            TaskKind::Delete,
            move |cancel, progress| {
                TaskOutput::Delete(manage::delete(
                    &destination,
                    &ids,
                    key.as_ref(),
                    cancel,
                    progress,
                ))
            },
        ));
        self.screen = Screen::Working;
    }

    pub fn ask_transfer(&mut self, snapshot: SnapshotInfo) {
        if self.is_busy() || !self.ensure_unlocked(&snapshot) {
            return;
        }
        let Some(folder) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let target =
            snapshots::chosen_destination(&folder, self.config.advanced.destination_app_folder);
        self.pending = Some(Pending::Transfer(Box::new(snapshot), target));
    }

    pub fn start_transfer(&mut self, ctx: &egui::Context, snapshot: SnapshotInfo, target: PathBuf) {
        if self.is_busy() {
            return;
        }
        let key = self.vault.key.clone();
        self.manage.current = Some((snapshot.id.clone(), target.clone()));
        self.task = Some(Task::spawn(
            ctx,
            TaskKind::Transfer,
            move |cancel, progress| {
                TaskOutput::Transfer(manage::transfer(
                    &snapshot,
                    &target,
                    key.as_ref(),
                    cancel,
                    progress,
                ))
            },
        ));
        self.screen = Screen::Working;
    }

    /// Copy all or some files of a backup to a folder the user picks.
    pub fn ask_extract(&mut self, snapshot: SnapshotInfo, files: Option<Vec<StoredFile>>) {
        if self.is_busy() || !self.ensure_read_access(&snapshot) {
            return;
        }
        let Some(folder) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let target = folder.join(crate::engine::sources::sanitize_segment(&snapshot.id));
        self.pending = Some(Pending::Extract {
            snapshot: Box::new(snapshot),
            files,
            target,
        });
    }

    pub fn start_extract(
        &mut self,
        ctx: &egui::Context,
        snapshot: SnapshotInfo,
        files: Option<Vec<StoredFile>>,
        target: PathBuf,
        open: bool,
    ) {
        if self.is_busy() || !self.ensure_read_access(&snapshot) {
            return;
        }
        let key = self.vault.key.clone();
        self.manage.extract_target = Some(target.clone());
        self.manage.current = Some((snapshot.id.clone(), target.clone()));
        self.task = Some(Task::spawn(
            ctx,
            TaskKind::Extract { open },
            move |cancel, progress| {
                let files = match files {
                    Some(files) => files,
                    None => match snapshots::contents(&snapshot, key.as_ref()) {
                        Ok(files) => files,
                        Err(err) => return TaskOutput::Extract(Err(err)),
                    },
                };
                TaskOutput::Extract(manage::extract(
                    &snapshot,
                    &files,
                    &target,
                    key.as_ref(),
                    cancel,
                    progress,
                ))
            },
        ));
        if !open {
            self.screen = Screen::Working;
        }
    }

    /// Decrypts or copies one file into a temporary folder and opens it with
    /// its usual program. The backup is never opened directly, so the program
    /// cannot change it.
    pub fn open_stored_file(
        &mut self,
        ctx: &egui::Context,
        snapshot: SnapshotInfo,
        file: StoredFile,
    ) {
        // A new folder only this user can open (no shared /tmp exposure).
        let target = match crate::platform::private_temp_dir(OPEN_PREFIX) {
            Ok(dir) => dir,
            Err(err) => {
                self.notify(NoticeKind::Error, err.to_string());
                return;
            }
        };
        self.start_extract(ctx, snapshot, Some(vec![file]), target, true);
    }

    pub fn browse_snapshot(&mut self, ctx: &egui::Context, snapshot: SnapshotInfo) {
        if self.is_busy() || !self.ensure_read_access(&snapshot) {
            return;
        }
        let key = self.vault.key.clone();
        self.task = Some(Task::spawn(ctx, TaskKind::LoadContents, move |_, _| {
            TaskOutput::Contents(
                snapshots::contents(&snapshot, key.as_ref()).map(|files| (snapshot, files)),
            )
        }));
        self.screen = Screen::Working;
    }

    /// Qualified ids the retention rules would remove now.
    pub fn retention_candidates(&self) -> Vec<String> {
        retention::to_remove(self.all_snapshots(), &self.config.retention, &self.computer)
    }

    /// When the retention rules would remove each backup, assuming backups
    /// continue as the switched-on jobs plan them (or once a day without jobs).
    pub fn retention_forecast(&mut self) -> std::rc::Rc<super::RetentionForecast> {
        use std::hash::{Hash, Hasher};
        let today = chrono::Local::now().date_naive();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", self.config.retention).hash(&mut hasher);
        format!("{:?}", self.config.schedules).hash(&mut hasher);
        today.hash(&mut hasher);
        for s in self.all_snapshots() {
            s.qualified_id().hash(&mut hasher);
            s.header.as_ref().map(|h| h.status as u8).hash(&mut hasher);
        }
        let fingerprint = hasher.finish();
        if let Some((cached, forecast)) = &self.manage.forecast
            && *cached == fingerprint
        {
            return forecast.clone();
        }

        let (future, assumed_daily) = crate::automatic::future_backup_times(&self.config);
        let removals = retention::forecast(
            self.all_snapshots(),
            &self.config.retention,
            &self.computer,
            &future,
        );
        let forecast = std::rc::Rc::new(super::RetentionForecast {
            removals,
            assumed_daily,
        });
        self.manage.forecast = Some((fingerprint, forecast.clone()));
        forecast
    }

    pub fn ask_clean_up(&mut self) {
        let ids = self.retention_candidates();
        let snapshots: Vec<SnapshotInfo> = self
            .all_snapshots()
            .iter()
            .filter(|s| ids.contains(&s.qualified_id()))
            .cloned()
            .collect();
        self.ask_delete(snapshots);
    }

    /// After a backup from the window: quietly remove old backups if the rules are on.
    pub(super) fn apply_retention_quietly(&mut self, ctx: &egui::Context) {
        if !self.config.retention.enabled || self.viewer.is_some() {
            return;
        }
        let destination = self.config.destination.clone();
        let policy = self.config.retention.clone();
        let computer = self.computer.clone();
        let key = self.vault.key.clone();
        self.manage_job = Some(Job::spawn(ctx, move |cancel, send| {
            send(crate::automatic::apply_retention(
                &destination,
                &policy,
                &computer,
                key.as_ref(),
                cancel,
            ));
        }));
    }

    pub(super) fn poll_manage_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.manage_job else {
            return;
        };
        let (mut values, finished) = job.drain();
        if finished {
            self.manage_job = None;
        }
        match values.pop() {
            Some(Some(Ok(report))) if !report.deleted.is_empty() => {
                self.record(crate::history::Event::BackupsDeleted {
                    snapshots: report.deleted.clone(),
                    by_rules: true,
                    message: String::new(),
                });
                self.notify(
                    NoticeKind::Info,
                    self.lang.retention_removed(report.deleted.len()),
                );
                self.refresh_snapshots(ctx);
            }
            Some(Some(Err(err))) => {
                tracing::warn!("old backups could not be removed: {err}");
                self.notify(NoticeKind::Warning, self.lang.error_message(&err));
            }
            _ => {}
        }
    }

    /// Handles the result of a management task.
    pub(super) fn finish_manage_task(
        &mut self,
        ctx: &egui::Context,
        kind: TaskKind,
        output: TaskOutput,
    ) {
        let lang = self.lang;
        let (snapshot, target) = self
            .manage
            .current
            .take()
            .map(|(s, t)| (s, t.display().to_string()))
            .unwrap_or_default();
        match &output {
            TaskOutput::Verify(Ok(report)) if !report.cancelled => {
                self.record(crate::history::Event::Verified {
                    snapshot,
                    files: report.files,
                    damaged: report.damaged.len() as u64,
                    missing: report.missing.len() as u64,
                    unreadable: report.unreadable.len() as u64,
                });
            }
            TaskOutput::Delete(Ok(report)) if !report.deleted.is_empty() => {
                self.record(crate::history::Event::BackupsDeleted {
                    snapshots: report.deleted.clone(),
                    by_rules: false,
                    message: report.warnings.first().cloned().unwrap_or_default(),
                });
            }
            TaskOutput::Transfer(Ok(_)) => {
                self.record(crate::history::Event::BackupMoved {
                    snapshot,
                    to: target,
                });
            }
            TaskOutput::Extract(Ok(report))
                if !matches!(kind, TaskKind::Extract { open: true }) && report.files > 0 =>
            {
                self.record(crate::history::Event::FilesCopied {
                    snapshot,
                    files: report.files,
                    to: target,
                });
            }
            _ => {}
        }
        match output {
            TaskOutput::Verify(result) => {
                self.present_done(Done::Verify(result.map_err(|e| lang.error_message(&e))));
            }
            TaskOutput::Delete(result) => {
                self.manage.checked.clear();
                self.present_done(Done::Delete(result.map_err(|e| lang.error_message(&e))));
                self.refresh_snapshots(ctx);
            }
            TaskOutput::Transfer(result) => {
                self.manage.checked.clear();
                self.present_done(Done::Transfer(result.map_err(|e| lang.error_message(&e))));
                self.refresh_snapshots(ctx);
            }
            TaskOutput::Extract(result) => {
                let open = matches!(kind, TaskKind::Extract { open: true });
                let target = self.manage.extract_target.take().unwrap_or_default();
                match (open, result) {
                    (true, Ok(report)) => match (&report.first, report.failed.first()) {
                        (Some(file), _) => platform::open_with_default_program(file),
                        (None, Some(message)) => self.notify(NoticeKind::Error, message.clone()),
                        (None, None) => {}
                    },
                    (true, Err(err)) => self.notify(NoticeKind::Error, lang.error_message(&err)),
                    (false, result) => {
                        self.present_done(Done::Extract(
                            result
                                .map(|report| (report, target))
                                .map_err(|e| lang.error_message(&e)),
                        ));
                    }
                }
            }
            TaskOutput::Contents(result) => match result {
                Ok((snapshot, files)) => {
                    self.screen = Screen::Browse(Box::new(super::views::browse::BrowseState::new(
                        snapshot, files,
                    )));
                }
                Err(crate::error::EngineError::Locked) => {
                    self.screen = Screen::Main;
                    self.open_unlock(AfterUnlock::Read);
                }
                Err(err) => {
                    self.screen = Screen::Main;
                    self.notify(NoticeKind::Warning, lang.error_message(&err));
                }
            },
            _ => {}
        }
    }
}
