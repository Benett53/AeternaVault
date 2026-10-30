//! Applications: programs found on this computer whose data folders are worth
//! keeping. Adding one puts its folders into the list of folders to back up,
//! where their contents can be seen and changed like any other folder.

use eframe::egui::{self, Align, FontFamily, Layout, Ui};

use crate::gui::theme::{SANS_STRONG, palette};
use crate::gui::widgets::{self, ButtonKind};
use crate::gui::{AeternaApp, DetectedApp};
use crate::platform::apps::Category;

pub fn show(app: &mut AeternaApp, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let t = app.lang.t();
    let lang = app.lang;
    let p = *palette(ui);

    if app.apps.detected.is_none() && app.apps.job.is_none() {
        app.refresh_apps(&ctx);
    }

    widgets::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| widgets::section_title_info(ui, t.apps_title, t.apps_hint));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if widgets::button(ui, ButtonKind::Quiet, t.refresh, app.apps.job.is_none())
                    .on_hover_text("F5")
                    .clicked()
                {
                    app.refresh_apps(&ctx);
                }
            });
        });
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::singleline(&mut app.apps.search)
                .hint_text(t.search)
                .desired_width(280.0),
        );
    });

    let Some(detected) = app.apps.detected.clone() else {
        ui.add_space(14.0);
        widgets::progress_line(ui, None);
        widgets::secondary_text(ui, t.loading);
        return;
    };
    if detected.is_empty() {
        ui.add_space(14.0);
        widgets::card(ui, |ui| widgets::secondary_text(ui, t.apps_none_found));
        return;
    }

    let needle = app.apps.search.trim().to_lowercase();
    let mut add: Option<DetectedApp> = None;
    let mut remove: Option<DetectedApp> = None;
    for category in Category::ALL {
        let rows: Vec<&DetectedApp> = detected
            .iter()
            .filter(|d| d.category == category)
            .filter(|d| {
                needle.is_empty()
                    || app
                        .catalog
                        .get(&d.id)
                        .is_some_and(|a| a.display_name(lang).to_lowercase().contains(&needle))
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        ui.add_space(14.0);
        widgets::card(ui, |ui| {
            widgets::section_title(ui, lang.category(category));
            for (index, detected) in rows.iter().enumerate() {
                let Some(def) = app.catalog.get(&detected.id) else {
                    continue;
                };
                let added = app.app_added(detected);
                if index > 0 {
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2((ui.available_width() - 220.0).max(200.0), 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(def.display_name(lang))
                                        .family(FontFamily::Name(SANS_STRONG.into()))
                                        .color(p.text),
                                );
                                if let Some(bytes) = detected.bytes {
                                    widgets::secondary_text(ui, lang.bytes(bytes));
                                }
                                if added {
                                    ui.label(egui::RichText::new(t.app_added).color(p.success));
                                }
                            });
                            if let Some(note) = def.note(lang) {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(note).color(p.text_secondary),
                                    )
                                    .wrap(),
                                );
                            }
                            for folder in &detected.folders {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(folder.display().to_string())
                                            .monospace()
                                            .size(13.0)
                                            .color(p.text_secondary),
                                    )
                                    .truncate(),
                                );
                            }
                        },
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if added {
                            if widgets::button(ui, ButtonKind::Quiet, t.remove_from_list, true)
                                .on_hover_text(t.remove_from_list_hint)
                                .clicked()
                            {
                                remove = Some((*detected).clone());
                            }
                        } else if widgets::button(ui, ButtonKind::Secondary, t.add_to_backup, true)
                            .on_hover_text(t.add_to_backup_hint)
                            .clicked()
                        {
                            add = Some((*detected).clone());
                        }
                    });
                });
            }
        });
    }
    ui.add_space(12.0);

    if let Some(detected) = remove {
        app.remove_app(&detected);
    }
    if let Some(detected) = add {
        app.add_app(&ctx, &detected);
    }
}
