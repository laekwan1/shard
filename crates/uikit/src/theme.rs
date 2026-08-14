//! Shared dark theme and Korean font loading.

use egui::{Color32, Context, FontData, FontDefinitions, FontFamily};
use std::sync::Arc;

/// egui's bundled fonts have no CJK coverage, so Korean UI strings render as
/// tofu without this. Malgun Gothic ships with every Windows 10/11 install;
/// the rest are fallbacks for trimmed images.
const KOREAN_FONTS: &[&str] = &[
    r"C:\Windows\Fonts\malgun.ttf",
    r"C:\Windows\Fonts\malgunsl.ttf",
    r"C:\Windows\Fonts\NanumGothic.ttf",
    r"C:\Windows\Fonts\NanumBarunGothic.ttf",
];

fn load_korean_font() -> Option<(String, Vec<u8>)> {
    for path in KOREAN_FONTS {
        if let Ok(bytes) = std::fs::read(path) {
            let name = std::path::Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("korean")
                .to_owned();
            tracing::debug!("using {path} for Korean glyphs");
            return Some((name, bytes));
        }
    }
    tracing::warn!("no Korean font found; Hangul will render as boxes");
    None
}

/// Install fonts, dark visuals and spacing. Call once from `App::new`.
pub fn install(ctx: &Context, accent: Color32) {
    if let Some((name, bytes)) = load_korean_font() {
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
        // Appended, not prepended: Latin keeps egui's crisper default and only
        // codepoints it lacks fall through to the system font.
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            if let Some(list) = fonts.families.get_mut(&family) {
                list.push(name.clone());
            }
        }
        ctx.set_fonts(fonts);
    }

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(17, 19, 24);
    visuals.window_fill = Color32::from_rgb(17, 19, 24);
    visuals.extreme_bg_color = Color32::from_rgb(11, 13, 17);
    visuals.faint_bg_color = Color32::from_rgb(24, 27, 34);
    visuals.hyperlink_color = accent;
    visuals.selection.bg_fill = accent.linear_multiply(0.35);
    visuals.selection.stroke.color = accent;
    visuals.widgets.hovered.bg_stroke.color = accent.linear_multiply(0.6);
    visuals.widgets.active.bg_stroke.color = accent;
    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 7.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.slider_width = 220.0;
    });
}

/// Muted label colour for secondary text and trade-off hints.
pub const MUTED: Color32 = Color32::from_rgb(138, 146, 162);
/// Positive state (connected, bypass working).
pub const GOOD: Color32 = Color32::from_rgb(52, 211, 153);
/// Degraded state.
pub const WARN: Color32 = Color32::from_rgb(251, 191, 36);
/// Failure state.
pub const BAD: Color32 = Color32::from_rgb(248, 113, 113);
