//! The host page's screen watermark (web `pdfcraft:watermark`): one diagonal text over each page,
//! drawn like WebViewer's `setWatermark({ diagonal })`. It lives on the screen only; nothing here
//! reaches the document or a saved file.
//!
//! The text is turned into glyph outlines here and handed to the renderer, which fills them into
//! the page picture between the page's contents and its annotations, so the annotations lie over
//! the watermark as in WebViewer (and a highlight still multiplies with it).

use std::sync::Arc;

use kurbo::{BezPath, Point};
use pdfcraft_render::ScreenWatermark;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider as _};

/// The longest text the host may set, in characters.
pub const MAX_TEXT_CHARS: usize = 200;
/// WebViewer draws `48pt` on a canvas in page points, and a canvas `pt` is 4/3 px: 64 page units.
const FONT_UNITS: f32 = 48.0 * 4.0 / 3.0;
/// The line height is `1.5 * fontSize` with the 48 taken as pixels, as WebViewer's.
const LINE_UNITS: f64 = 1.5 * 48.0;
/// `rgba(120, 120, 120, 0.3)`.
const RGB: [f32; 3] = [120.0 / 255.0; 3];
const ALPHA: f32 = 0.3;

/// Clean a text from the host: at most [`MAX_TEXT_CHARS`] characters, none for blank. `None` clears.
pub(crate) fn clean(text: Option<&str>) -> Option<String> {
    text.filter(|t| !t.trim().is_empty()).map(|t| t.chars().take(MAX_TEXT_CHARS).collect())
}

/// Appends a glyph's outline to a path, moved to `origin` (y down) from skrifa's y-up em units.
struct Pen<'a> {
    path: &'a mut BezPath,
    x: f64,
    baseline: f64,
}

impl Pen<'_> {
    fn at(&self, x: f32, y: f32) -> Point {
        Point::new(self.x + f64::from(x), self.baseline - f64::from(y))
    }
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.path.move_to(p);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.path.line_to(p);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, p) = (self.at(cx0, cy0), self.at(x, y));
        self.path.quad_to(c, p);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (c0, c1, p) = (self.at(cx0, cy0), self.at(cx1, cy1), self.at(x, y));
        self.path.curve_to(c0, c1, p);
    }
    fn close(&mut self) {
        self.path.close_path();
    }
}

/// The interface's faces in the order egui looks a character up in them.
fn faces(defs: &egui::FontDefinitions) -> Vec<FontRef<'_>> {
    let names = defs.families.get(&egui::FontFamily::Proportional).map(Vec::as_slice).unwrap_or_default();
    names
        .iter()
        .filter_map(|name| defs.font_data.get(name))
        .filter_map(|data| FontRef::from_index(&data.font, data.index).ok())
        .collect()
}

/// The watermark for `text` (lines split on `\n`): each line centred on the origin with its top
/// `(n - lines / 2) * 72` points below it, upright, in the interface's fonts at 64 points.
/// `None` when no character of the text has a glyph.
pub(crate) fn build(text: &str) -> Option<Arc<ScreenWatermark>> {
    let defs = crate::theme::font_definitions();
    let faces = faces(&defs);
    let size = Size::new(FONT_UNITS);
    let ascent = f64::from(faces.first()?.metrics(size, LocationRef::default()).ascent);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut outline = BezPath::new();
    for (n, line) in lines.iter().enumerate() {
        // Each character from the first face that has it, laid out left to right.
        let mut glyphs = Vec::new();
        let mut width = 0.0f64;
        for c in line.chars() {
            let Some((face, gid)) = faces.iter().find_map(|f| f.charmap().map(c).map(|g| (f, g))) else { continue };
            let advance = f64::from(face.glyph_metrics(size, LocationRef::default()).advance_width(gid).unwrap_or(0.0));
            glyphs.push((face, gid, width));
            width += advance;
        }
        let top = (n as f64 - lines.len() as f64 / 2.0) * LINE_UNITS;
        for (face, gid, x) in glyphs {
            let Some(glyph) = face.outline_glyphs().get(gid) else { continue };
            let mut pen = Pen { path: &mut outline, x: x - width / 2.0, baseline: top + ascent };
            // A glyph that fails to draw leaves at most a partial shape; nothing else is lost.
            let _ = glyph.draw(DrawSettings::unhinted(size, LocationRef::default()), &mut pen);
        }
    }
    (!outline.elements().is_empty()).then(|| Arc::new(ScreenWatermark { key: text.to_string(), outline, rgb: RGB, alpha: ALPHA }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Shape;

    #[test]
    fn blank_or_missing_text_clears_and_long_text_is_cut() {
        assert_eq!(clean(None), None);
        assert_eq!(clean(Some("  \n ")), None);
        assert_eq!(clean(Some("기밀")), Some("기밀".to_string()));
        assert_eq!(clean(Some(&"가".repeat(500))).map(|t| t.chars().count()), Some(MAX_TEXT_CHARS));
    }

    #[test]
    fn the_outline_is_centred_on_the_origin_and_lines_stack_like_webviewer() {
        let one = build("CONFIDENTIAL").expect("latin text has glyphs").outline.bounding_box();
        assert!((one.center().x).abs() < 8.0, "{one:?}");
        // 64 points of font: capitals are about 46 points tall, so the line is not tiny or huge.
        assert!(one.height() > 30.0 && one.height() < 70.0, "{one:?}");
        let two = build("A\nA").expect("latin text has glyphs").outline.bounding_box();
        // Two lines 72 points apart.
        assert!((two.height() - (one.height().min(70.0) + 72.0)).abs() < 40.0, "{two:?}");
        assert!(two.height() > 72.0, "{two:?}");
    }

    #[test]
    fn korean_text_has_outlines_and_an_unmappable_text_has_none() {
        // Hangul has a face only in builds that embed craft-fonts.
        if !pdfcraft_fonts::ui_korean_fonts().is_empty() {
            assert!(build("기밀 문서").is_some());
        }
        assert!(build("\n").is_none());
    }
}
