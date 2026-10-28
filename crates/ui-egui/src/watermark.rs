//! The host page's screen watermark (web `pdfcraft:watermark`): one diagonal text over each page,
//! drawn like WebViewer's `setWatermark({ diagonal })`. It lives on the screen only; nothing here
//! reaches the document or a saved file.

use egui::{Color32, FontId, Painter, Pos2, Rect, Vec2, vec2};

/// The longest text the host may set, in characters.
pub const MAX_TEXT_CHARS: usize = 200;
/// WebViewer draws `48pt` on a canvas in page points, and a canvas `pt` is 4/3 px: 64 page units.
const FONT_UNITS: f32 = 48.0 * 4.0 / 3.0;
/// The line height is `1.5 * fontSize` with the 48 taken as pixels, as WebViewer's.
const LINE_UNITS: f32 = 1.5 * 48.0;
/// `rgba(120, 120, 120, 0.3)`.
const COLOR: Color32 = Color32::from_rgba_premultiplied(36, 36, 36, 77);
/// The font size is laid out in steps of 2^(1/16) (4.4%) within these limits, so a zoom does not
/// put a new glyph size into the font atlas on every frame, and none is huge.
const MIN_FONT: f32 = 4.0;
const MAX_FONT: f32 = 512.0;

/// Clean a text from the host: at most [`MAX_TEXT_CHARS`] characters, none for blank. `None` clears.
pub(crate) fn clean(text: Option<&str>) -> Option<String> {
    text.filter(|t| !t.trim().is_empty()).map(|t| t.chars().take(MAX_TEXT_CHARS).collect())
}

/// Where the lines go for a page `shown` on screen, `page_w` points wide as shown: the font size
/// in screen points, the clockwise angle, and the top of each line relative to the page centre
/// before rotation (WebViewer: `(n - lines / 2) * lineHeight`).
pub(crate) fn plan(shown: Vec2, page_w: f32, lines: usize) -> (f32, f32, Vec<f32>) {
    let scale = shown.x / page_w.max(1.0);
    let size = (FONT_UNITS * scale).clamp(MIN_FONT, MAX_FONT);
    let size = (size.log2() * 16.0).round() / 16.0;
    // The page's own diagonal, counter-clockwise on screen.
    let angle = -(shown.y / shown.x.max(1.0)).atan();
    let tops = (0..lines).map(|n| (n as f32 - lines as f32 / 2.0) * LINE_UNITS * scale).collect();
    (2f32.powf(size), angle, tops)
}

/// Turn `local` (an offset from `centre`, before rotation) by `angle` radians clockwise around `centre`.
pub(crate) fn rotate_about(centre: Pos2, angle: f32, local: Vec2) -> Pos2 {
    let (s, c) = angle.sin_cos();
    centre + vec2(local.x * c - local.y * s, local.x * s + local.y * c)
}

/// Draw the watermark over the page that fills `rect` on screen, `page_w` points wide as shown.
pub(crate) fn paint(painter: &Painter, rect: Rect, page_w: f32, text: &str) {
    let lines: Vec<&str> = text.split('\n').collect();
    let (size, angle, tops) = plan(rect.size(), page_w, lines.len());
    // Past a page's edge nothing shows, as on WebViewer's page canvas.
    let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    for (line, top) in lines.into_iter().zip(tops) {
        if line.is_empty() {
            continue;
        }
        let galley = painter.layout_no_wrap(line.to_string(), FontId::proportional(size), COLOR);
        let local = vec2(-galley.size().x / 2.0, top);
        let pos = rotate_about(rect.center(), angle, local);
        painter.add(egui::epaint::TextShape::new(pos, galley, COLOR).with_angle(angle));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn blank_or_missing_text_clears_and_long_text_is_cut() {
        assert_eq!(clean(None), None);
        assert_eq!(clean(Some("  \n ")), None);
        assert_eq!(clean(Some("기밀")), Some("기밀".to_string()));
        assert_eq!(clean(Some(&"가".repeat(500))).map(|t| t.chars().count()), Some(MAX_TEXT_CHARS));
    }

    #[test]
    fn the_text_follows_the_pages_diagonal_and_grows_with_the_zoom() {
        // A 600 x 800 pt page at 100% and at 200%.
        let (s1, a1, _) = plan(vec2(600.0, 800.0), 600.0, 1);
        let (s2, a2, _) = plan(vec2(1200.0, 1600.0), 600.0, 1);
        assert!((a1 + (800.0f32 / 600.0).atan()).abs() < 1e-6, "{a1}");
        assert_eq!(a1, a2);
        // 48 pt is 64 page units; the size follows the zoom within one step (4.4%).
        assert!((s1 / 64.0 - 1.0).abs() < 0.023, "{s1}");
        assert!((s2 / 128.0 - 1.0).abs() < 0.023, "{s2}");
    }

    #[test]
    fn the_size_stays_in_limits_and_in_steps() {
        let (tiny, ..) = plan(vec2(10.0, 10.0), 600.0, 1);
        let (huge, ..) = plan(vec2(60000.0, 80000.0), 600.0, 1);
        assert!((tiny - MIN_FONT).abs() < 1e-3 && (huge - MAX_FONT).abs() < 1e-2, "{tiny} {huge}");
        // Two zooms a hair apart share one size.
        assert_eq!(plan(vec2(600.0, 800.0), 600.0, 1).0, plan(vec2(601.0, 801.3), 600.0, 1).0);
    }

    #[test]
    fn lines_stack_around_the_centre_like_webviewer() {
        let (_, _, one) = plan(vec2(600.0, 800.0), 600.0, 1);
        let (_, _, two) = plan(vec2(600.0, 800.0), 600.0, 2);
        assert_eq!(one, vec![-36.0]);
        assert_eq!(two, vec![-72.0, 0.0]);
    }

    #[test]
    fn rotating_about_the_centre_keeps_the_distance_and_turns_clockwise() {
        let c = pos2(100.0, 100.0);
        let p = rotate_about(c, std::f32::consts::FRAC_PI_2, vec2(10.0, 0.0));
        assert!((p.x - 100.0).abs() < 1e-4 && (p.y - 110.0).abs() < 1e-4, "{p:?}");
    }
}
