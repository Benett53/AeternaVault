//! Main view: what is kept safe (folders and application settings), where,
//! how, and when it happens automatically.

use std::path::{Path, PathBuf};

use eframe::egui::{
    self, Align, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, Pos2, Sense, Stroke, Ui,
};

use super::status_color;
use crate::engine::selection::{self, CheckState};
use crate::gui::theme::{SANS_STRONG, palette};
use crate::gui::widgets::{self, ButtonKind};
use crate::gui::{AeternaApp, View};

const TREE_ROW: f32 = 26.0;
const TREE_LIMIT: usize = 400;

pub fn show(app: &mut AeternaApp, ui: &mut Ui) {
    folders_card(app, ui);
    ui.add_space(14.0);
    destination_card(app, ui);
    ui.add_space(12.0);
}

// ---------------------------------------------------------------------------
// Folders
// ---------------------------------------------------------------------------

fn folders_card(app: &mut AeternaApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let t = app.lang.t();
    let lang = app.lang;
    let p = *palette(ui);

    widgets::card(ui, |ui| {
        widgets::section_title(ui, t.sources_title);

        if app.config.sources.is_empty() {
            widgets::secondary_text(ui, t.sources_empty);
        }

        let mut remove = None;
        let mut changed = false;
        let row_count = app.config.sources.len();
        for index in 0..row_count {
            let size = app.size_of(&app.config.sources[index].path).cloned();
            let path = app.config.sources[index].path.clone();
            let expanded = app.tree.expanded_sources.contains(&path);
            let width = ui.available_width();
            let (row_rect, row_response) =
                ui.allocate_exact_size(egui::vec2(width, 50.0), Sense::click());
            // A folder that was just added (e.g. from Applications) scrolls into view.
            if app
                .tree
                .reveal
                .as_ref()
                .is_some_and(|r| crate::engine::paths_equal(r, &path))
            {
                ui.scroll_to_rect(row_rect, Some(Align::TOP));
                app.tree.reveal = None;
            }
            let mut row = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(row_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );

            // Chevron to open the contents tree.
            let (chevron_rect, chevron) =
                row.allocate_exact_size(egui::vec2(20.0, 30.0), Sense::click());
            paint_chevron(
                &row,
                chevron_rect,
                expanded,
                chevron.hovered() || row_response.hovered(),
            );
            let source_name = app.config.sources[index].display_name();
            chevron.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!("{}: {source_name}", t.choose_contents),
                )
            });
            let chevron = chevron.on_hover_text(t.choose_contents);

            let source = &mut app.config.sources[index];
            let mixed = source.enabled && source.is_partial();
            let checkbox =
                row.add(egui::Checkbox::new(&mut source.enabled, "").indeterminate(mixed));
            let enabled_now = source.enabled;
            checkbox.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    true,
                    enabled_now,
                    &source_name,
                )
            });
            if checkbox.changed() {
                changed = true;
            }
            if app.config.encryption.selected() {
                let state = selection::mark_state(&source.encrypt_paths, &source.plain_paths, ".");
                if widgets::lock_toggle(
                    &mut row,
                    state,
                    &format!("{}: {source_name}", t.encrypt_item),
                )
                .clicked()
                {
                    selection::toggle_mark(&mut source.encrypt_paths, &mut source.plain_paths, ".");
                    changed = true;
                }
            }

            let text_width = (width - 250.0).max(120.0);
            let text_left = row.cursor().left();
            let name_rect = egui::Rect::from_min_size(
                egui::pos2(text_left, row_rect.top() + 5.0),
                egui::vec2(text_width, 20.0),
            );
            let path_rect = egui::Rect::from_min_size(
                egui::pos2(text_left, row_rect.top() + 25.0),
                egui::vec2(text_width, 18.0),
            );
            let name_color = if source.enabled {
                p.text
            } else {
                p.text_secondary
            };
            widgets::paint_cell(
                &row,
                name_rect,
                &source.display_name(),
                FontId::new(15.0, FontFamily::Name(SANS_STRONG.into())),
                name_color,
                Align::Min,
            );
            widgets::paint_cell(
                &row,
                path_rect,
                &source.path.display().to_string(),
                FontId::proportional(12.5),
                p.text_secondary,
                Align::Min,
            );

            row.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::button(ui, ButtonKind::Quiet, "×", true)
                    .on_hover_text(t.remove_source)
                    .clicked()
                {
                    remove = Some(index);
                }
                ui.add_space(6.0);
                let size_text = match size {
                    Some(Some((bytes, _))) => lang.bytes(bytes),
                    Some(None) => t.source_missing.to_string(),
                    None => t.calculating.to_string(),
                };
                let color = if matches!(size, Some(None)) {
                    p.warning
                } else {
                    p.text_secondary
                };
                ui.label(egui::RichText::new(size_text).color(color).size(13.5));
            });

            let name_clicked = row_response.clicked()
                && row_response
                    .interact_pointer_pos()
                    .is_some_and(|pos| pos.x > text_left && pos.x < text_left + text_width);
            if chevron.clicked() || name_clicked {
                if expanded {
                    app.tree.expanded_sources.remove(&path);
                } else {
                    app.tree.expanded_sources.insert(path.clone());
                }
            }

            if expanded {
                ui.add_space(4.0);
                tree_panel(app, ui, index);
            }

            if index + 1 < row_count {
                let y = ui.cursor().top() + 3.0;
                ui.painter().line_segment(
                    [
                        egui::pos2(row_rect.left(), y),
                        egui::pos2(row_rect.right(), y),
                    ],
                    Stroke::new(1.0, p.border),
                );
                ui.add_space(6.0);
            }
        }
        if let Some(index) = remove {
            let removed = app.config.sources.remove(index);
            app.tree.expanded_sources.remove(&removed.path);
            tracing::info!("source removed from list: {}", removed.path.display());
            changed = true;
        }
        if changed {
            app.mark_dirty();
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if widgets::button(ui, ButtonKind::Secondary, t.add_folder, true)
                .on_hover_text(t.drop_hint)
                .clicked()
                && let Some(folder) = rfd::FileDialog::new().pick_folder()
            {
                app.add_folder(&ctx, folder);
            }
            if widgets::button(ui, ButtonKind::Quiet, t.add_app_data, true)
                .on_hover_text(t.add_app_data_hint)
                .clicked()
            {
                app.navigate(&ctx, View::Apps);
            }
        });
    });
}

fn paint_chevron(ui: &Ui, rect: egui::Rect, open: bool, hovered: bool) {
    let p = palette(ui);
    let c = rect.center();
    let color = if hovered { p.text } else { p.text_secondary };
    let points = if open {
        vec![
            c + egui::vec2(-4.5, -2.0),
            c + egui::vec2(0.0, 2.5),
            c + egui::vec2(4.5, -2.0),
        ]
    } else {
        vec![
            c + egui::vec2(-2.0, -4.5),
            c + egui::vec2(2.5, 0.0),
            c + egui::vec2(-2.0, 4.5),
        ]
    };
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(1.5, color)));
}

/// The "choose contents" tree below a source row.
fn tree_panel(app: &mut AeternaApp, ui: &mut Ui, index: usize) {
    let ctx = ui.ctx().clone();
    let t = app.lang.t();
    let p = *palette(ui);
    let root = app.config.sources[index].path.clone();
    let mut changed = false;

    Frame::new()
        .fill(p.background)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let source = &mut app.config.sources[index];
                if widgets::button(ui, ButtonKind::Quiet, t.select_all, true).clicked() {
                    selection::select_all(&mut source.include_paths, &mut source.exclude_paths);
                    changed = true;
                }
                if widgets::button(ui, ButtonKind::Quiet, t.select_none, true).clicked() {
                    selection::select_none(&mut source.include_paths, &mut source.exclude_paths);
                    changed = true;
                }
                if source.is_partial() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        widgets::secondary_text(ui, t.partial_selection);
                    });
                }
            });

            if !root.is_dir() {
                widgets::secondary_text(ui, t.source_missing);
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt(("tree", root.display().to_string()))
                .max_height(320.0)
                .min_scrolled_height(260.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    if render_dir(app, ui, index, &root, "", 0) {
                        changed = true;
                    }
                });
        });

    if changed {
        app.mark_dirty();
        app.invalidate_size(&ctx, &root);
    }
}

/// Draws the entries of one folder; returns `true` if the selection changed.
fn render_dir(
    app: &mut AeternaApp,
    ui: &mut Ui,
    index: usize,
    root: &Path,
    rel_dir: &str,
    depth: usize,
) -> bool {
    let p = *palette(ui);
    let t = app.lang.t();
    let lang = app.lang;
    let dir: PathBuf = if rel_dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel_dir.replace('/', "\\"))
    };
    let entries = app.tree_listing(&dir);
    let mut changed = false;
    let mut marks_changed = false;

    if entries.is_empty() && depth == 0 {
        widgets::secondary_text(ui, t.folder_empty);
    }

    for entry in entries.iter().take(TREE_LIMIT) {
        let rel = if rel_dir.is_empty() {
            entry.name.clone()
        } else {
            format!("{rel_dir}/{}", entry.name)
        };
        let open_key = (root.to_path_buf(), rel.clone());
        let is_open = entry.is_dir && app.tree.open_dirs.contains(&open_key);
        let state = app.config.sources[index].selection().state(&rel);

        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(width, TREE_ROW), Sense::hover());
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(3), p.raised);
        }
        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        row.add_space(depth as f32 * 18.0);

        let (arrow_rect, arrow) =
            row.allocate_exact_size(egui::vec2(18.0, TREE_ROW), Sense::click());
        if entry.is_dir {
            paint_chevron(&row, arrow_rect, is_open, arrow.hovered());
            if arrow.clicked() {
                if is_open {
                    app.tree.open_dirs.remove(&open_key);
                } else {
                    app.tree.open_dirs.insert(open_key.clone());
                }
            }
        }

        let mut checked = state == CheckState::Checked;
        let checkbox = row
            .add(egui::Checkbox::new(&mut checked, "").indeterminate(state == CheckState::Mixed));
        checkbox.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Checkbox,
                true,
                state == CheckState::Checked,
                &entry.name,
            )
        });
        if checkbox.clicked() {
            let source = &mut app.config.sources[index];
            selection::toggle(&mut source.include_paths, &mut source.exclude_paths, &rel);
            changed = true;
        }
        if app.config.encryption.selected() {
            let source = &mut app.config.sources[index];
            let mark = selection::mark_state(&source.encrypt_paths, &source.plain_paths, &rel);
            if widgets::lock_toggle(
                &mut row,
                mark,
                &format!("{}: {}", t.encrypt_item, entry.name),
            )
            .clicked()
            {
                selection::toggle_mark(&mut source.encrypt_paths, &mut source.plain_paths, &rel);
                marks_changed = true;
            }
        }

        paint_entry_icon(&row, entry.is_dir);
        let name_color = if state == CheckState::Unchecked {
            p.text_secondary
        } else {
            p.text
        };
        let left = row.cursor().left() + 22.0;
        let name_rect = egui::Rect::from_min_max(
            egui::pos2(left, rect.top()),
            egui::pos2(rect.right() - 90.0, rect.bottom()),
        );
        widgets::paint_cell(
            ui,
            name_rect,
            &entry.name,
            FontId::proportional(13.5),
            name_color,
            Align::Min,
        );
        if !entry.is_dir {
            let size_rect = egui::Rect::from_min_max(
                egui::pos2(rect.right() - 86.0, rect.top()),
                egui::pos2(rect.right() - 6.0, rect.bottom()),
            );
            widgets::paint_cell(
                ui,
                size_rect,
                &lang.bytes(entry.size),
                FontId::proportional(12.5),
                p.text_secondary,
                Align::Max,
            );
        }

        if is_open && render_dir(app, ui, index, root, &rel, depth + 1) {
            changed = true;
        }
    }
    if entries.len() > TREE_LIMIT {
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 18.0 + 40.0);
            widgets::secondary_text(ui, lang.more_items(entries.len() - TREE_LIMIT));
        });
    }
    if marks_changed {
        // Marks do not change the size, only where things are stored.
        app.mark_dirty();
    }
    changed
}

/// Minimal line icons: a folder tab or a document with a folded corner.
fn paint_entry_icon(ui: &Ui, is_dir: bool) {
    let p = palette(ui);
    let origin = ui.cursor().left_top() + egui::vec2(2.0, (TREE_ROW - 12.0) / 2.0);
    let stroke = Stroke::new(1.2, p.text_secondary);
    let painter = ui.painter();
    if is_dir {
        let body = egui::Rect::from_min_size(origin + egui::vec2(0.0, 2.0), egui::vec2(15.0, 10.0));
        painter.rect_stroke(
            body,
            CornerRadius::same(2),
            stroke,
            egui::StrokeKind::Inside,
        );
        painter.line_segment(
            [origin + egui::vec2(1.0, 2.0), origin + egui::vec2(6.0, 2.0)],
            stroke,
        );
        painter.line_segment(
            [origin + egui::vec2(1.5, 0.5), origin + egui::vec2(5.5, 0.5)],
            stroke,
        );
    } else {
        let points = vec![
            origin + egui::vec2(2.0, 0.0),
            origin + egui::vec2(9.0, 0.0),
            origin + egui::vec2(12.0, 3.0),
            origin + egui::vec2(12.0, 12.0),
            origin + egui::vec2(2.0, 12.0),
        ];
        painter.add(egui::Shape::closed_line(points, stroke));
        let _ = Pos2::ZERO;
    }
}

// ---------------------------------------------------------------------------
// Destination and encryption (the details live in Settings)
// ---------------------------------------------------------------------------

fn destination_card(app: &mut AeternaApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let t = app.lang.t();
    let p = *palette(ui);

    widgets::card(ui, |ui| {
        widgets::section_title(ui, t.destination_title);
        ui.horizontal(|ui| {
            let destination = app.config.destination.display().to_string();
            let not_set = destination.is_empty();
            if not_set {
                widgets::secondary_text(ui, t.destination_not_set);
            } else {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(destination)
                            .family(FontFamily::Name(SANS_STRONG.into()))
                            .color(p.text),
                    )
                    .truncate(),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if not_set {
                    if widgets::button(ui, ButtonKind::Secondary, t.choose, !app.is_busy())
                        .clicked()
                    {
                        app.choose_destination(&ctx);
                    }
                } else if widgets::button(ui, ButtonKind::Quiet, t.change_in_settings, true)
                    .clicked()
                {
                    app.navigate(&ctx, View::Settings);
                }
            });
        });
        destination_status(app, ui);

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let mut on = app.config.encryption.enabled;
            if widgets::toggle(ui, &mut on, !app.is_busy(), t.encrypt_backups) {
                app.set_encryption(on);
            }
            ui.add_space(4.0);
            ui.vertical(|ui| {
                widgets::strong_text(ui, t.encrypt_backups);
                let hint = if app.config.encryption.selected() {
                    t.enc_status_selected
                } else if app.config.encryption.enabled {
                    t.encrypt_hint_on
                } else {
                    t.encrypt_hint_off
                };
                widgets::secondary_text(ui, hint);
            });
        });
        if app.config.encryption.selected() {
            let marked = app
                .config
                .sources
                .iter()
                .filter(|s| s.enabled && !s.encrypt_paths.is_empty())
                .count();
            ui.horizontal_wrapped(|ui| {
                ui.add_space(52.0);
                widgets::lock_icon(
                    ui,
                    ui.cursor().left_center() + egui::vec2(6.0, 9.0),
                    p.accent,
                );
                ui.add_space(16.0);
                if marked == 0 {
                    ui.label(
                        egui::RichText::new(t.scope_nothing_marked)
                            .size(13.0)
                            .color(p.warning),
                    );
                } else {
                    widgets::secondary_text(ui, t.scope_selected_short);
                }
            });
        }
    });
}

/// "Not reachable" or the free space at the destination.
pub fn destination_status(app: &AeternaApp, ui: &mut Ui) {
    let t = app.lang.t();
    let p = *palette(ui);
    match app.destination_reachable() {
        Some(false) => {
            ui.label(
                egui::RichText::new(t.destination_unreachable)
                    .color(p.warning)
                    .size(13.0),
            );
        }
        Some(true) => match (app.destination_free, app.destination_total) {
            (Some(free), Some(total)) if total > 0 => {
                let used = 1.0 - free as f32 / total as f32;
                widgets::usage_bar(
                    ui,
                    used,
                    &format!("{} · {}", app.lang.free_space(free), app.lang.bytes(total)),
                );
            }
            (Some(free), _) => {
                widgets::secondary_text(ui, app.lang.free_space(free));
            }
            _ => {}
        },
        None => {}
    }
    if app.destination_shares_drive() {
        ui.add(
            egui::Label::new(
                egui::RichText::new(t.destination_same_drive)
                    .color(p.warning)
                    .size(13.0),
            )
            .wrap(),
        );
    }
}

/// Always-visible bar at the bottom: last backup on the left, actions on the right.
pub fn action_bar(app: &mut AeternaApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let t = app.lang.t();
    let lang = app.lang;
    let p = *palette(ui);

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            widgets::secondary_text(ui, t.last_backup_title);
            ui.horizontal(|ui| match app.last_backup() {
                None => {
                    ui.label(t.last_backup_none);
                }
                Some(info) => {
                    let status = info.header.as_ref().map(|h| h.status);
                    widgets::status_dot(ui, status_color(&p, status));
                    match &info.header {
                        Some(header) => {
                            let when =
                                lang.relative_time(header.started_at.with_timezone(&chrono::Local));
                            ui.label(format!("{when} — {}", lang.status(status)));
                        }
                        None if info.is_locked() => {
                            ui.label(format!("{} — {}", info.id, t.encrypted_label));
                        }
                        None => {
                            ui.label(format!("{} — {}", info.id, lang.status(None)));
                        }
                    }
                }
            });
        });

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let ready = !app.is_busy() && app.config.enabled_sources().next().is_some();
            if widgets::button(ui, ButtonKind::Primary, t.back_up_now, ready)
                .on_hover_text("Ctrl+B")
                .clicked()
            {
                app.plan_backup(&ctx);
            }
            if widgets::button(ui, ButtonKind::Quiet, t.restore_ellipsis, true).clicked() {
                app.navigate(&ctx, View::Restore);
            }
        });
    });
}
