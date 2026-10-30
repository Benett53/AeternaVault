//! Navigation history, keyboard shortcuts and mouse conveniences.
//!
//! * Back / forward: the side buttons of the mouse, Alt+Left / Alt+Right.
//! * Tabs: Ctrl+Tab / Ctrl+Shift+Tab, Ctrl+1 … Ctrl+7.
//! * Ctrl+B back up now, F5 refresh, Ctrl+, settings, Esc leaves a sub-page.
//! * Pressing the mouse wheel starts auto-scrolling: move the mouse away from
//!   the starting point to scroll, click or press Esc to stop.

use eframe::egui::{self, Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase};

use super::{AeternaApp, Screen, View};

#[derive(Default)]
pub struct Navigation {
    back: Vec<View>,
    forward: Vec<View>,
    /// Auto-scrolling started with the middle mouse button: anchor and pointer.
    autoscroll: Option<(Pos2, Pos2)>,
}

impl Navigation {
    pub fn autoscroll_anchor(&self) -> Option<Pos2> {
        self.autoscroll.map(|(anchor, _)| anchor)
    }
}

/// Speed of auto-scrolling in points per frame and point of distance.
const AUTOSCROLL_SPEED: f32 = 0.18;
/// Distance from the anchor that does not scroll yet.
const AUTOSCROLL_DEAD_ZONE: f32 = 6.0;

impl AeternaApp {
    /// Goes to a tab and remembers where we came from.
    pub fn navigate(&mut self, ctx: &egui::Context, view: View) {
        if !self.view_available(view) {
            return;
        }
        let leaving_sub_page = !matches!(self.screen, Screen::Main | Screen::Working);
        if self.view == view && !leaving_sub_page && !matches!(self.screen, Screen::Working) {
            return;
        }
        if self.view != view {
            self.nav.back.push(self.view);
            self.nav.forward.clear();
        }
        self.show_view(ctx, view);
    }

    fn show_view(&mut self, ctx: &egui::Context, view: View) {
        self.view = view;
        self.screen = Screen::Main;
        if matches!(view, View::Restore | View::Backups) {
            self.refresh_snapshots(ctx);
        }
        if view == View::Apps {
            self.refresh_apps(ctx);
        }
    }

    pub fn go_back(&mut self, ctx: &egui::Context) {
        // A sub-page (browsing, a result) closes first.
        if matches!(self.screen, Screen::Browse(_) | Screen::Done(_)) && self.task.is_none() {
            self.screen = Screen::Main;
            return;
        }
        if let Some(view) = self.nav.back.pop() {
            self.nav.forward.push(self.view);
            self.show_view(ctx, view);
        }
    }

    pub fn go_forward(&mut self, ctx: &egui::Context) {
        if let Some(view) = self.nav.forward.pop() {
            self.nav.back.push(self.view);
            self.show_view(ctx, view);
        }
    }

    /// Tabs that exist right now (the viewer shows only the backups).
    pub fn view_available(&self, view: View) -> bool {
        self.viewer.is_none() || matches!(view, View::Restore | View::Backups)
    }

    pub fn visible_views(&self) -> Vec<View> {
        View::ALL
            .into_iter()
            .filter(|v| self.view_available(*v))
            .collect()
    }

    fn cycle_view(&mut self, ctx: &egui::Context, step: isize) {
        let views = self.visible_views();
        let Some(index) = views.iter().position(|v| *v == self.view) else {
            return;
        };
        let next = (index as isize + step).rem_euclid(views.len() as isize) as usize;
        self.navigate(ctx, views[next]);
    }

    /// Keyboard shortcuts and the extra mouse buttons. Runs once per frame
    /// before the views, so fields and dialogs still get every other key.
    pub(super) fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let dialog_open =
            self.pending.is_some() || self.vault.dialog.is_some() || self.schedule.editor.is_some();
        let (back, forward) = ctx.input(|i| {
            (
                i.pointer.button_pressed(PointerButton::Extra1),
                i.pointer.button_pressed(PointerButton::Extra2),
            )
        });
        if dialog_open {
            return;
        }
        let consume = |mods: Modifiers, key: Key| ctx.input_mut(|i| i.consume_key(mods, key));

        if back || consume(Modifiers::ALT, Key::ArrowLeft) {
            self.go_back(ctx);
        }
        if forward || consume(Modifiers::ALT, Key::ArrowRight) {
            self.go_forward(ctx);
        }
        if consume(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab) {
            self.cycle_view(ctx, -1);
        } else if consume(Modifiers::CTRL, Key::Tab) {
            self.cycle_view(ctx, 1);
        }
        let numbers = [
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
        ];
        for (index, key) in numbers.into_iter().enumerate() {
            if consume(Modifiers::CTRL, key)
                && let Some(view) = self.visible_views().get(index).copied()
            {
                self.navigate(ctx, view);
            }
        }
        if consume(Modifiers::CTRL, Key::Comma) {
            self.navigate(ctx, View::Settings);
        }
        if consume(Modifiers::NONE, Key::F5) {
            self.refresh_vault();
            self.refresh_snapshots(ctx);
            self.refresh_apps(ctx);
        }
        if consume(Modifiers::CTRL, Key::B) && self.viewer.is_none() && !self.is_busy() {
            self.plan_backup(ctx);
        }
        let nothing_focused = ctx.memory(|m| m.focused().is_none());
        if nothing_focused && self.nav.autoscroll.is_none() && consume(Modifiers::NONE, Key::Escape)
        {
            self.go_back(ctx);
        }
    }

    /// Turns a pressed mouse wheel into continuous scrolling (like in browsers).
    pub(super) fn autoscroll_hook(&mut self, raw: &mut egui::RawInput) {
        let mut stop = false;
        for event in &raw.events {
            match event {
                Event::PointerButton {
                    button: PointerButton::Middle,
                    pressed: true,
                    pos,
                    ..
                } => {
                    if self.nav.autoscroll.take().is_none() {
                        self.nav.autoscroll = Some((*pos, *pos));
                    }
                }
                Event::PointerButton {
                    button: PointerButton::Primary | PointerButton::Secondary,
                    pressed: true,
                    ..
                }
                | Event::PointerGone
                | Event::Key {
                    key: Key::Escape,
                    pressed: true,
                    ..
                } => stop = true,
                Event::PointerMoved(pos) => {
                    if let Some((_, current)) = &mut self.nav.autoscroll {
                        *current = *pos;
                    }
                }
                Event::WindowFocused(false) => stop = true,
                _ => {}
            }
        }
        if stop {
            self.nav.autoscroll = None;
        }
        let Some((anchor, current)) = self.nav.autoscroll else {
            return;
        };
        let offset = current - anchor;
        let shape = |v: f32| {
            let magnitude = (v.abs() - AUTOSCROLL_DEAD_ZONE).max(0.0);
            -v.signum() * magnitude * AUTOSCROLL_SPEED
        };
        let delta = egui::vec2(shape(offset.x), shape(offset.y));
        if delta != egui::Vec2::ZERO {
            raw.events.push(Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta,
                phase: TouchPhase::Move,
                modifiers: Modifiers::NONE,
            });
        }
    }

    /// The anchor symbol while auto-scrolling.
    pub(super) fn paint_autoscroll(&self, ctx: &egui::Context) {
        let Some(anchor) = self.nav.autoscroll_anchor() else {
            return;
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("autoscroll"),
        ));
        let dark = ctx.theme() == egui::Theme::Dark;
        let p = super::theme::palette_for(dark);
        painter.circle(
            anchor,
            13.0,
            p.panel.gamma_multiply(0.92),
            egui::Stroke::new(1.2, p.text_secondary),
        );
        painter.circle_filled(anchor, 2.5, p.text_secondary);
        for dir in [-1.0f32, 1.0] {
            let tip = anchor + egui::vec2(0.0, 8.5 * dir);
            let base = anchor + egui::vec2(0.0, 4.5 * dir);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    tip,
                    base + egui::vec2(-3.5, 0.0),
                    base + egui::vec2(3.5, 0.0),
                ],
                p.text_secondary,
                egui::Stroke::NONE,
            ));
        }
        ctx.set_cursor_icon(egui::CursorIcon::AllScroll);
        ctx.request_repaint();
    }
}
