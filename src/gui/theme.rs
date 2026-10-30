//! The AeternaVault look: calm, dark or light grounds with one gold accent
//! (taken from the logo).
//!
//! * Appearances: light, dark, and black for OLED screens (pure black
//!   background, so unused pixels stay off).
//! * Type: a serif for headings and a humanist sans for text, taken from the
//!   system (Georgia and Segoe UI on Windows; DejaVu, Noto or Liberation on
//!   Linux). Custom fonts can be set in the configuration file.
//!
//! One palette per appearance (light, dark, black for OLED).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow,
    Stroke, TextStyle, Theme, ThemePreference, Visuals, style::WidgetVisuals,
};

use crate::config::{Appearance, Fonts};

pub const SERIF: &str = "serif";
pub const SERIF_ITALIC: &str = "serif-italic";
pub const SANS_STRONG: &str = "sans-strong";

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub background: Color32,
    pub panel: Color32,
    pub raised: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_secondary: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const DARK: Palette = Palette {
    background: hex(0x14161A),
    panel: hex(0x1C1F24),
    raised: hex(0x252930),
    border: hex(0x30353D),
    text: hex(0xEDE6D6),
    text_secondary: hex(0xA29D92),
    accent: hex(0xE0B43C),
    accent_hover: hex(0xF0C955),
    on_accent: hex(0x14161A),
    // Slightly lighter than the fill tones so they stay readable as text.
    success: hex(0x8FB27A),
    warning: hex(0xD9A145),
    error: hex(0xD2705F),
};

/// Pure black for OLED screens.
pub const BLACK: Palette = Palette {
    background: hex(0x000000),
    panel: hex(0x0B0B0C),
    raised: hex(0x161618),
    border: hex(0x2A2A2D),
    text: hex(0xEDE6D6),
    text_secondary: hex(0x9E998F),
    accent: hex(0xE0B43C),
    accent_hover: hex(0xF0C955),
    on_accent: hex(0x000000),
    success: hex(0x8FB27A),
    warning: hex(0xD9A145),
    error: hex(0xD2705F),
};

pub const LIGHT: Palette = Palette {
    background: hex(0xF6F2E9),
    panel: hex(0xFCF9F3),
    raised: hex(0xFFFEFA),
    border: hex(0xE2D9C8),
    text: hex(0x1C1F24),
    text_secondary: hex(0x635F56),
    accent: hex(0x8A6410),
    accent_hover: hex(0xA97D18),
    on_accent: hex(0xFCF9F3),
    success: hex(0x4E6B38),
    warning: hex(0x8C5F14),
    error: hex(0x9A3F32),
};

/// Whether the dark theme is currently the black (OLED) variant.
static BLACK_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn palette(ui: &egui::Ui) -> &'static Palette {
    palette_for(ui.visuals().dark_mode)
}

pub fn palette_for(dark: bool) -> &'static Palette {
    match (dark, BLACK_ACTIVE.load(Ordering::Relaxed)) {
        (true, true) => &BLACK,
        (true, false) => &DARK,
        (false, _) => &LIGHT,
    }
}

pub fn apply(ctx: &egui::Context, appearance: Appearance) {
    let black = appearance == Appearance::Black;
    BLACK_ACTIVE.store(black, Ordering::Relaxed);
    ctx.set_visuals_of(
        Theme::Dark,
        visuals(if black { &BLACK } else { &DARK }, true),
    );
    ctx.set_visuals_of(Theme::Light, visuals(&LIGHT, false));
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(12.0, 11.0);
        style.spacing.button_padding = egui::vec2(18.0, 8.0);
        style.spacing.interact_size.y = 32.0;
        style.spacing.window_margin = Margin::same(24);
        style.spacing.indent = 24.0;
        style.spacing.icon_width = 18.0;
        style.spacing.icon_width_inner = 10.0;
        style.spacing.icon_spacing = 9.0;
        style.spacing.combo_width = 140.0;
        style.animation_time = 0.18;
        // Short explanations appear after resting on a setting for a moment.
        style.interaction.tooltip_delay = 0.45;
        style.interaction.show_tooltips_only_when_still = true;
        style.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(27.0, FontFamily::Name(SERIF.into())),
            ),
            (TextStyle::Body, FontId::new(16.5, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(16.5, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(14.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(15.0, FontFamily::Monospace),
            ),
        ]
        .into();
    });
    ctx.set_theme(match appearance {
        Appearance::System => ThemePreference::System,
        Appearance::Dark | Appearance::Black => ThemePreference::Dark,
        Appearance::Light => ThemePreference::Light,
    });
}

fn visuals(p: &Palette, dark: bool) -> Visuals {
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let radius = CornerRadius::same(4);

    v.panel_fill = p.background;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.window_shadow = Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 110 } else { 36 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 80 } else { 24 }),
    };
    v.extreme_bg_color = p.background;
    v.text_edit_bg_color = Some(p.background);
    v.faint_bg_color = p.raised;
    v.code_bg_color = p.raised;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.error;
    v.selection.bg_fill = p.accent.gamma_multiply(if dark { 0.30 } else { 0.22 });
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.striped = false;
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;

    let widget = |bg: Color32, stroke: Color32, fg: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0, stroke),
        corner_radius: radius,
        fg_stroke: Stroke::new(1.0, fg),
        expansion: 0.0,
    };
    v.widgets.noninteractive = widget(p.panel, p.border, p.text);
    v.widgets.inactive = widget(p.raised, p.border, p.text);
    v.widgets.hovered = widget(p.raised, p.accent, p.text);
    v.widgets.active = widget(p.raised, p.accent_hover, p.text);
    v.widgets.open = widget(p.raised, p.accent, p.text);
    v
}

/// Installs heading and body fonts. Missing files are simply skipped; egui's
/// built-in fonts always remain as fallback (they also cover symbols).
pub fn install_fonts(ctx: &egui::Context, overrides: &Fonts) {
    let mut fonts = FontDefinitions::default();
    let windows_fonts = std::env::var_os("WINDIR")
        .map(|w| PathBuf::from(w).join("Fonts"))
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\Fonts"));
    let linux = |parts: &[&str]| -> PathBuf {
        parts
            .iter()
            .fold(PathBuf::from("/usr/share/fonts"), |acc, p| acc.join(p))
    };

    let mut load = |name: &str, candidates: &[PathBuf]| -> bool {
        for path in candidates {
            if let Some(data) = read_font(path) {
                fonts
                    .font_data
                    .insert(name.to_string(), Arc::new(FontData::from_owned(data)));
                return true;
            }
        }
        false
    };

    let heading_candidates: Vec<PathBuf> = overrides
        .heading
        .iter()
        .cloned()
        .chain([
            windows_fonts.join("georgia.ttf"),
            windows_fonts.join("constan.ttf"),
            linux(&["truetype", "dejavu", "DejaVuSerif.ttf"]),
            linux(&["dejavu", "DejaVuSerif.ttf"]),
            linux(&["truetype", "noto", "NotoSerif-Regular.ttf"]),
            linux(&["noto", "NotoSerif-Regular.ttf"]),
            linux(&["truetype", "liberation", "LiberationSerif-Regular.ttf"]),
            linux(&["liberation-serif", "LiberationSerif-Regular.ttf"]),
        ])
        .collect();
    let body_candidates: Vec<PathBuf> = overrides
        .body
        .iter()
        .cloned()
        .chain([
            windows_fonts.join("segoeui.ttf"),
            linux(&["truetype", "noto", "NotoSans-Regular.ttf"]),
            linux(&["noto", "NotoSans-Regular.ttf"]),
            linux(&["truetype", "dejavu", "DejaVuSans.ttf"]),
            linux(&["dejavu", "DejaVuSans.ttf"]),
        ])
        .collect();
    let strong_candidates = [
        windows_fonts.join("seguisb.ttf"),
        linux(&["truetype", "noto", "NotoSans-SemiBold.ttf"]),
        linux(&["noto", "NotoSans-SemiBold.ttf"]),
        linux(&["truetype", "dejavu", "DejaVuSans-Bold.ttf"]),
        linux(&["dejavu", "DejaVuSans-Bold.ttf"]),
    ];

    let has_serif = load("av-serif", &heading_candidates);
    let has_serif_italic = load("av-serif-italic", &[windows_fonts.join("georgiai.ttf")]);
    let has_sans = load("av-sans", &body_candidates);
    let has_sans_strong = load("av-sans-strong", &strong_candidates);
    // Check marks and similar signs, which the text fonts may lack.
    let has_symbols = load(
        "av-symbols",
        &[
            windows_fonts.join("seguisym.ttf"),
            linux(&["truetype", "dejavu", "DejaVuSans.ttf"]),
            linux(&["dejavu", "DejaVuSans.ttf"]),
        ],
    );

    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let family = |primary: &[(&str, bool)]| -> Vec<String> {
        primary
            .iter()
            .chain(&[("av-symbols", has_symbols)])
            .filter(|(_, ok)| *ok)
            .map(|(n, _)| n.to_string())
            .chain(defaults.iter().cloned())
            .collect()
    };

    let proportional = family(&[("av-sans", has_sans)]);
    let serif = family(&[("av-serif", has_serif), ("av-sans", has_sans)]);
    let serif_italic = family(&[
        ("av-serif-italic", has_serif_italic),
        ("av-serif", has_serif),
    ]);
    let strong = family(&[("av-sans-strong", has_sans_strong), ("av-sans", has_sans)]);

    fonts
        .families
        .insert(FontFamily::Proportional, proportional);
    fonts.families.insert(FontFamily::Name(SERIF.into()), serif);
    fonts
        .families
        .insert(FontFamily::Name(SERIF_ITALIC.into()), serif_italic);
    fonts
        .families
        .insert(FontFamily::Name(SANS_STRONG.into()), strong);
    ctx.set_fonts(fonts);
}

fn read_font(path: &Path) -> Option<Vec<u8>> {
    let data = std::fs::read(path).ok()?;
    // Very small sanity check for TrueType / OpenType signatures.
    let valid = data.len() > 4 && matches!(&data[..4], [0, 1, 0, 0] | b"OTTO" | b"true");
    if !valid {
        tracing::warn!(
            "ignoring font {}: not a TrueType/OpenType file",
            path.display()
        );
    }
    valid.then_some(data)
}
