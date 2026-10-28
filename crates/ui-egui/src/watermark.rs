//! The host page's screen watermark (web `pdfcraft:watermark`): one diagonal text over each page,
//! drawn like WebViewer's `setWatermark({ diagonal })`. It lives on the screen only; nothing here
//! reaches the document or a saved file.

use egui::emath::Rot2;
use egui::epaint::{Mesh, Vertex};
use egui::{Color32, FontId, Painter, Pos2, Rect, Shape, Vec2, vec2};

/// The longest text the host may set, in characters.
pub const MAX_TEXT_CHARS: usize = 200;
/// WebViewer draws `48pt` on a canvas in page points, and a canvas `pt` is 4/3 px: 64 page units.
const FONT_UNITS: f32 = 48.0 * 4.0 / 3.0;
/// The line height is `1.5 * fontSize` with the 48 taken as pixels, as WebViewer's.
const LINE_UNITS: f32 = 1.5 * 48.0;
/// `rgba(120, 120, 120, 0.3)`.
const COLOR: Color32 = Color32::from_rgba_premultiplied(36, 36, 36, 77);
/// Glyphs are laid out at a size in steps of 2^(1/4) between these limits (so a zoom does not put
/// a new size into the font atlas on every frame, and none is huge), and the mesh is scaled to the
/// exact size, which is [`FONT_UNITS`] times the zoom.
const MIN_LAYOUT: f32 = 8.0;
const MAX_LAYOUT: f32 = 256.0;
/// Below this the text is not readable and nothing is drawn.
const MIN_DRAWN: f32 = 2.0;

/// Clean a text from the host: at most [`MAX_TEXT_CHARS`] characters, none for blank. `None` clears.
pub(crate) fn clean(text: Option<&str>) -> Option<String> {
    text.filter(|t| !t.trim().is_empty()).map(|t| t.chars().take(MAX_TEXT_CHARS).collect())
}

/// Where the lines go for a page `shown` on screen, `page_w` points wide as shown: the font size
/// in screen points, the clockwise angle, and the top of each line relative to the page centre
/// before rotation (WebViewer: `(n - lines / 2) * lineHeight`).
pub(crate) fn plan(shown: Vec2, page_w: f32, lines: usize) -> (f32, f32, Vec<f32>) {
    let scale = shown.x / page_w.max(1.0);
    // The page's own diagonal, counter-clockwise on screen.
    let angle = -(shown.y / shown.x.max(1.0)).atan();
    let tops = (0..lines).map(|n| (n as f32 - lines as f32 / 2.0) * LINE_UNITS * scale).collect();
    (FONT_UNITS * scale, angle, tops)
}

/// The size the glyphs are laid out at for a font of `size`: the next step up, within the limits.
pub(crate) fn layout_size(size: f32) -> f32 {
    2f32.powf(((size.max(1.0).log2() * 4.0).ceil() / 4.0).clamp(MIN_LAYOUT.log2(), MAX_LAYOUT.log2()))
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
    if !size.is_finite() || size < MIN_DRAWN {
        return;
    }
    // Past a page's edge nothing shows, as on WebViewer's page canvas.
    let painter = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let (laid, rot) = (layout_size(size), Rot2::from_angle(angle));
    let k = size / laid;
    let tex = painter.ctx().fonts(|f| f.font_image_size());
    let uv = vec2(1.0 / tex[0] as f32, 1.0 / tex[1] as f32);
    let mut mesh = Mesh::default();
    for (line, top) in lines.into_iter().zip(tops) {
        if line.is_empty() {
            continue;
        }
        let galley = painter.layout_no_wrap(line.to_string(), FontId::proportional(laid), COLOR);
        // The line centred on the page centre with its top `top` above or below it, turned about the centre.
        let origin = rotate_about(rect.center(), angle, vec2(-galley.size().x * k / 2.0, top));
        for row in &galley.rows {
            let first = mesh.vertices.len() as u32;
            mesh.indices.extend(row.visuals.mesh.indices.iter().map(|i| i + first));
            mesh.vertices.extend(row.visuals.mesh.vertices.iter().map(|v| Vertex {
                pos: origin + rot * ((row.pos.to_vec2() + v.pos.to_vec2()) * k),
                uv: (v.uv.to_vec2() * uv).to_pos2(),
                color: if v.color == Color32::PLACEHOLDER { COLOR } else { v.color },
            }));
        }
    }
    if !mesh.is_empty() {
        painter.add(Shape::mesh(mesh));
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
    fn the_text_follows_the_pages_diagonal_and_grows_exactly_with_the_zoom() {
        // A 600 x 800 pt page at 100%, 200% and 3200%: 48 pt is 64 page units, with no cap or rounding.
        let size = |zoom: f32| plan(vec2(600.0 * zoom, 800.0 * zoom), 600.0, 1);
        let (s1, a1, _) = size(1.0);
        assert!((a1 + (800.0f32 / 600.0).atan()).abs() < 1e-6, "{a1}");
        assert_eq!((s1, size(2.0).0, size(32.0).0), (64.0, 128.0, 2048.0));
        assert_eq!(a1, size(32.0).1);
    }

    #[test]
    fn glyphs_are_laid_out_in_steps_within_limits_and_scaled_to_the_exact_size() {
        assert_eq!(layout_size(64.0), 64.0);
        assert_eq!(layout_size(1.0), MIN_LAYOUT);
        assert_eq!(layout_size(2048.0), MAX_LAYOUT);
        // Never smaller than asked (so it is only scaled down) and at most one step (19%) bigger.
        for size in [9.0f32, 70.0, 99.0, 130.0, 250.0] {
            let l = layout_size(size);
            assert!(l >= size && l <= size * 2f32.powf(0.25) + 1e-3, "{size} -> {l}");
        }
        // Two zooms a hair apart share one laid out size.
        assert_eq!(layout_size(65.0), layout_size(66.0));
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
