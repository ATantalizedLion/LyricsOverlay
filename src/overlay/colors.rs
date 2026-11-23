//! Color helpers that keep the UI legible on any theme: accent and gray-text colors
//! held at a minimum contrast against the background, and egui visuals to match.
use egui::Color32;

use crate::settings::Settings;

/// Minimum perceived-brightness difference (see `luminance`) kept between the accent and
/// the background.
const MIN_ACCENT_CONTRAST: f32 = 90.0;
/// Minimum perceived-brightness difference kept between plain gray text and the
/// background. Lower than the accent's, since some hints are meant to be dim.
const MIN_TEXT_CONTRAST: f32 = 60.0;

/// The accent color for UI chrome (icons, titles, selected widgets): the theme's own
/// accent if it has one, otherwise derived from the current-line color - either way
/// pushed away from the background if it would be hard to see on it.
pub(super) fn theme_accent(settings: &Settings) -> Color32 {
    let base = settings.accent_color.map_or_else(
        || derived_accent(settings.current_line_color),
        |[r, g, b]| Color32::from_rgb(r, g, b),
    );
    ensure_contrast(base, settings.background_color, MIN_ACCENT_CONTRAST)
}

/// Blends a theme color partway toward neutral gray, for themes without an explicit
/// accent: hints at the theme without becoming as loud as the fully-saturated lyric
/// colors.
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
fn derived_accent(base: [u8; 3]) -> Color32 {
    const NEUTRAL: f32 = 170.0;
    const BLEND: f32 = 0.4;
    // Always in 0..=255: a weighted average of two values already in that range.
    let mix = |c: u8| (f32::from(c).mul_add(BLEND, NEUTRAL * (1.0 - BLEND))) as u8;
    Color32::from_rgb(mix(base[0]), mix(base[1]), mix(base[2]))
}

/// Perceived brightness of an `[r, g, b]` color, 0 (black) to 255 (white).
fn luminance(color: [u8; 3]) -> f32 {
    0.299 * f32::from(color[0]) + 0.587 * f32::from(color[1]) + 0.114 * f32::from(color[2])
}

fn luminance32(color: Color32) -> f32 {
    luminance([color.r(), color.g(), color.b()])
}

/// Whether `background` counts as light, i.e. wants dark text and light widget chrome.
pub(super) fn is_light(background: [u8; 3]) -> bool {
    luminance(background) >= 128.0
}

/// `color` if its brightness differs from `background`'s by at least `min_contrast`,
/// otherwise `color` mixed toward black (light background) or white (dark background)
/// just far enough to get there - keeping as much of its hue as possible.
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
pub(super) fn ensure_contrast(color: Color32, background: [u8; 3], min_contrast: f32) -> Color32 {
    let bg = luminance(background);
    let current = luminance32(color);
    if (current - bg).abs() >= min_contrast {
        return color;
    }
    let (target, wanted) = if is_light(background) {
        (0.0, (bg - min_contrast).max(0.0))
    } else {
        (255.0, (bg + min_contrast).min(255.0))
    };
    // Luminance is linear in the mix, so solve for how far toward `target` to go.
    let span = target - current;
    let t = if span.abs() < f32::EPSILON {
        1.0
    } else {
        ((wanted - current) / span).clamp(0.0, 1.0)
    };
    let mix = |c: u8| (f32::from(c) + (target - f32::from(c)) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(mix(color.r()), mix(color.g()), mix(color.b()), color.a())
}

/// A gray that stays legible against `background`, for text that isn't itself
/// theme-colored (section labels, hints, slider/text-edit contents). `value_on_dark` is
/// the gray value that was tuned by eye assuming a near-black background; on a bright
/// background it's mirrored (`255 - value_on_dark`) instead. The switch between the two
/// happens over a narrow band around mid-brightness (rather than a gradual blend, which
/// put every gray at ~127 on a ~128 background), and the result is then held at least
/// `MIN_TEXT_CONTRAST` away from the background for the backgrounds inside that band.
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
pub(super) fn readable_gray(background: [u8; 3], value_on_dark: u8) -> Color32 {
    let x = luminance(background) / 255.0;
    let t = smoothstep(0.4, 0.6, x);
    let dark_value = f32::from(value_on_dark);
    let value = dark_value + (255.0 - 2.0 * dark_value) * t;
    ensure_contrast(
        Color32::from_gray(value.round().clamp(0.0, 255.0) as u8),
        background,
        MIN_TEXT_CONTRAST,
    )
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Black or white, whichever stays legible on top of `fill` - for text/icons drawn on an
/// accent-filled (selected) widget, where an accent-colored label would vanish.
pub(super) fn text_on(fill: Color32) -> Color32 {
    if luminance32(fill) < 128.0 {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

/// Base `egui::Visuals` for a panel painted directly on `background`: dark widget chrome
/// (text-edit fill, slider track, scrollbar, ...) for a dark background, light chrome for
/// a light one, so those don't default to dark-on-dark or light-on-light for a custom
/// theme. The selection highlight (checked/selected widget backgrounds) is tied to
/// `accent` instead of egui's default blue, so it actually reflects the active theme,
/// with selected widgets' text contrasting against that accent fill.
pub(super) fn theme_visuals(background: [u8; 3], accent: Color32) -> egui::Visuals {
    let base = if is_light(background) {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    egui::Visuals {
        selection: egui::style::Selection {
            bg_fill: accent,
            stroke: egui::Stroke::new(1.0f32, text_on(accent)),
        },
        ..base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: Color32) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }

    fn contrast(c: Color32, background: [u8; 3]) -> f32 {
        (luminance32(c) - luminance(background)).abs()
    }

    /// Every gray background from black to white.
    fn gray_backgrounds() -> impl Iterator<Item = [u8; 3]> {
        (0..=255u8).map(|v| [v, v, v])
    }

    #[test]
    fn readable_gray_keeps_contrast_on_every_background() {
        for background in gray_backgrounds().chain([[20, 8, 5], [255, 240, 200], [90, 160, 60]])
        {
            for value in [70, 100, 130, 160, 200, 210] {
                let gray = readable_gray(background, value);
                assert!(
                    contrast(gray, background) >= MIN_TEXT_CONTRAST - 1.0,
                    "gray {value} on {background:?} -> {gray:?}"
                );
            }
        }
    }

    #[test]
    fn readable_gray_unchanged_at_extremes() {
        assert_eq!(readable_gray([0, 0, 0], 160), Color32::from_gray(160));
        assert_eq!(readable_gray([255, 255, 255], 160), Color32::from_gray(95));
    }

    #[test]
    fn ensure_contrast_leaves_good_colors_alone() {
        let c = Color32::from_rgb(200, 60, 70);
        assert_eq!(ensure_contrast(c, [0, 0, 0], MIN_ACCENT_CONTRAST), c);
    }

    #[test]
    fn ensure_contrast_pushes_away_from_background() {
        for background in gray_backgrounds() {
            for color in [
                Color32::WHITE,
                Color32::BLACK,
                Color32::from_rgb(204, 204, 204),
                Color32::from_rgb(180, 165, 230),
            ] {
                let fixed = ensure_contrast(color, background, MIN_ACCENT_CONTRAST);
                assert!(
                    contrast(fixed, background) >= MIN_ACCENT_CONTRAST - 1.0,
                    "{color:?} on {background:?} -> {fixed:?}"
                );
            }
        }
    }

    #[test]
    fn accent_readable_on_light_background() {
        let settings = Settings {
            background_color: [250, 250, 250],
            current_line_color: [255, 255, 255],
            accent_color: None,
            ..Settings::default()
        };
        let accent = theme_accent(&settings);
        assert!(contrast(accent, settings.background_color) >= MIN_ACCENT_CONTRAST - 1.0);
    }

    #[test]
    fn accent_uses_theme_field_or_derives() {
        let mut settings = Settings {
            accent_color: Some([200, 60, 70]),
            ..Settings::default()
        };
        assert_eq!(rgb(theme_accent(&settings)), [200, 60, 70]);
        settings.accent_color = None;
        assert_eq!(theme_accent(&settings), derived_accent(settings.current_line_color));
    }

    #[test]
    fn builtin_accents_need_no_adjusting() {
        for theme in &crate::theming::BUILTIN_THEMES {
            let accent = theme.accent.expect("built-in themes set an accent");
            let c = Color32::from_rgb(accent[0], accent[1], accent[2]);
            assert_eq!(
                ensure_contrast(c, theme.background, MIN_ACCENT_CONTRAST),
                c,
                "{}",
                theme.name
            );
        }
    }

    #[test]
    fn text_on_picks_contrasting() {
        assert_eq!(text_on(Color32::from_rgb(250, 250, 200)), Color32::BLACK);
        assert_eq!(text_on(Color32::from_rgb(40, 20, 60)), Color32::WHITE);
    }
}
