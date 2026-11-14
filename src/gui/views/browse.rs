//! Looking inside a backup without restoring it: search, open single files
//! (a temporary copy), or copy chosen files to a folder.

use std::collections::HashSet;

use eframe::egui::{self, Align, CornerRadius, FontFamily, FontId, Layout, Sense, Ui};

use crate::engine::from_unix_nanos;
use crate::engine::snapshots::{SnapshotInfo, StoredFile};
use crate::gui::theme::{SERIF, palette};
use crate::gui::widgets::{self, ButtonKind};
use crate::gui::{AeternaApp, Screen};

pub struct BrowseState {
    pub snapshot: SnapshotInfo,
    pub files: Vec<StoredFile>,
    pub filter: String,
    /// Indexes into `files`.
    pub checked: HashSet<usize>,
}

impl BrowseState {
    pub fn new(snapshot: SnapshotInfo, files: Vec<StoredFile>) -> Self {
        Self {
            snapshot,
            files,
            filter: String::new(),
            checked: HashSet::new(),
        }
    }
}

enum Action {
    Back,
    Open(usize),
    CopyChecked,
    CopyAll,
}

pub fn show(app: &mut AeternaApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let t = app.lang.t();
    let lang = app.lang;
    let p = *palette(ui);
    let busy = app.is_busy();
    let (title, detail) = {
        let Screen::Browse(state) = &app.screen else {
            return;
        };
        super::backups::snapshot_details(app, &state.snapshot)
    };
    let Screen::Browse(state) = &mut app.screen else {
        return;
    };
    let mut action = None;

    widgets::centered_column(ui, widgets::COLUMN, |ui| {
        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                if widgets::button(ui, ButtonKind::Quiet, t.back_arrow, true).clicked() {
                    action = Some(Action::Back);
                }
                ui.label(
                    egui::RichText::new(lang.contents_of(&title))
                        .family(FontFamily::Name(SERIF.into()))
                        .size(24.0),
                );
            });
            widgets::secondary_text(ui, detail);
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                ui.add(
                    widgets::text_field(&mut state.filter)
                        .hint_text(t.filter_hint)
                        .desired_width(280.0),
                );
                if busy {
                    ui.add(egui::Spinner::new().size(16.0));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let bytes: u64 = state.files.iter().map(|f| f.size).sum();
                    widgets::secondary_text(
                        ui,
                        format!(
                            "{} · {}",
                            lang.files(state.files.len() as u64),
                            lang.bytes(bytes)
                        ),
                    );
                });
            });
            ui.add_space(6.0);

            let needle = state.filter.trim().to_lowercase();
            let visible: Vec<usize> = state
                .files
                .iter()
                .enumerate()
                .filter(|(_, f)| {
                    needle.is_empty() || f.display_path().to_lowercase().contains(&needle)
                })
                .map(|(i, _)| i)
                .collect();

            // Leave room for the buttons below the list, whatever the window size.
            let reserve = if state.snapshot.needs_key() {
                110.0
            } else {
                86.0
            };
            let height = (ui.available_height() - reserve).max(90.0);
            egui::ScrollArea::vertical()
                .id_salt("browse-files")
                .max_height(height)
                .min_scrolled_height(height)
                .auto_shrink([false, true])
                .show_rows(ui, 32.0, visible.len(), |ui, range| {
                    for &index in &visible[range] {
                        let file = &state.files[index];
                        let width = ui.available_width();
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(width, 32.0), Sense::hover());
                        if response.hovered() {
                            ui.painter()
                                .rect_filled(rect, CornerRadius::same(3), p.raised);
                        }
                        let mut row = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(rect.shrink2(egui::vec2(6.0, 0.0)))
                                .layout(Layout::left_to_right(Align::Center)),
                        );
                        let mut on = state.checked.contains(&index);
                        let path = file.display_path();
                        let checkbox = row.add(egui::Checkbox::without_text(&mut on));
                        checkbox.widget_info(|| {
                            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, &path)
                        });
                        if checkbox.changed() {
                            if on {
                                state.checked.insert(index);
                            } else {
                                state.checked.remove(&index);
                            }
                        }
                        let left = row.cursor().left() + 4.0;
                        if file.encrypted {
                            widgets::lock_icon(
                                ui,
                                egui::pos2(left + 6.0, rect.center().y),
                                p.accent,
                            );
                        }
                        let name_right = rect.right() - 280.0;
                        widgets::paint_cell(
                            ui,
                            egui::Rect::from_min_max(
                                egui::pos2(left + 18.0, rect.top()),
                                egui::pos2(name_right, rect.bottom()),
                            ),
                            &path,
                            FontId::proportional(13.5),
                            p.text,
                            Align::Min,
                        );
                        widgets::paint_cell(
                            ui,
                            egui::Rect::from_min_max(
                                egui::pos2(name_right + 8.0, rect.top()),
                                egui::pos2(name_right + 88.0, rect.bottom()),
                            ),
                            &lang.bytes(file.size),
                            FontId::proportional(12.5),
                            p.text_secondary,
                            Align::Max,
                        );
                        let modified: chrono::DateTime<chrono::Local> =
                            from_unix_nanos(file.modified).into();
                        widgets::paint_cell(
                            ui,
                            egui::Rect::from_min_max(
                                egui::pos2(name_right + 100.0, rect.top()),
                                egui::pos2(name_right + 210.0, rect.bottom()),
                            ),
                            &lang.datetime(modified),
                            FontId::proportional(12.5),
                            p.text_secondary,
                            Align::Min,
                        );
                        let mut buttons = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(rect.shrink2(egui::vec2(6.0, 1.0)))
                                .layout(Layout::right_to_left(Align::Center)),
                        );
                        if widgets::button(&mut buttons, ButtonKind::Quiet, t.open, !busy)
                            .on_hover_text(t.open_copy_hint)
                            .clicked()
                        {
                            action = Some(Action::Open(index));
                        }
                    }
                });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if widgets::button(ui, ButtonKind::Quiet, t.back, true).clicked() {
                    action = Some(Action::Back);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::button(ui, ButtonKind::Secondary, t.copy_all_to, !busy).clicked() {
                        action = Some(Action::CopyAll);
                    }
                    let label = lang.copy_n_to(state.checked.len());
                    if widgets::button(
                        ui,
                        ButtonKind::Secondary,
                        &label,
                        !busy && !state.checked.is_empty(),
                    )
                    .clicked()
                    {
                        action = Some(Action::CopyChecked);
                    }
                });
            });
            if state.snapshot.needs_key() {
                ui.add_space(4.0);
                widgets::secondary_text(ui, t.copies_are_decrypted);
            }
        });
    });

    let Some(action) = action else {
        return;
    };
    let Screen::Browse(state) = &app.screen else {
        return;
    };
    let snapshot = state.snapshot.clone();
    match action {
        Action::Back => app.screen = Screen::Main,
        Action::Open(index) => {
            let file = state.files[index].clone();
            app.open_stored_file(&ctx, snapshot, file);
        }
        Action::CopyChecked => {
            let mut chosen: Vec<usize> = state.checked.iter().copied().collect();
            chosen.sort_unstable();
            let files = chosen.into_iter().map(|i| state.files[i].clone()).collect();
            app.ask_extract(snapshot, Some(files));
        }
        Action::CopyAll => app.ask_extract(snapshot, None),
    }
}
