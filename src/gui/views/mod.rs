//! One module per screen. Each exposes `show(app, ui)`.

pub mod activity;
pub mod apps;
pub mod backup;
pub mod backups;
pub mod browse;
pub mod jobs;
pub mod notifications;
pub mod restore;
pub mod settings;
pub mod working;

use eframe::egui::Color32;

use super::theme::Palette;
use crate::engine::manifest::SnapshotStatus;

pub fn status_color(p: &Palette, status: Option<SnapshotStatus>) -> Color32 {
    match status {
        Some(SnapshotStatus::Complete) => p.success,
        Some(SnapshotStatus::CompleteWithWarnings) => p.warning,
        Some(SnapshotStatus::Cancelled) | None => p.text_secondary,
        Some(SnapshotStatus::Failed) => p.error,
    }
}
