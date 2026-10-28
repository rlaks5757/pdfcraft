//! Compare files: choose the older version, then the Compare panel lists the differences
//! (replaced blue, inserted green, deleted red), shades them on the newer document's pages, and
//! offers a report or the differences as comments.

use egui::{Align, Color32, Layout};
use pdfcraft_engine::DocId;
use pdfcraft_engine::compare::{Comparison, Kind};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, RightPanel, widgets};

/// The last comparison.
pub struct CompareState {
    pub old: DocId,
    pub new: DocId,
    pub result: Comparison,
    /// Regions that look different (page, user-space box), page n against page n.
    pub visual: Vec<(usize, [f64; 4])>,
    pub selected: Option<usize>,
    /// Side by side: the old file on the left, the new on the right, scrolled and zoomed
    /// together.
    pub side_by_side: bool,
    /// Which half leads the other (the one the pointer was over last): 0 old, 1 new.
    pub leader: usize,
    /// Each half's scroll offset last frame, to tell which one moved.
    pub prev_scroll: [egui::Vec2; 2],
    /// The PPM (host-lock) build: the viewer's compare screen (see [`ppm`]).
    pub ppm: bool,
    /// PPM: the overlay is shown instead of the two halves.
    pub overlay_shown: bool,
    /// PPM: the overlay was asked for and is built on the next frame, after the status showed.
    pub overlay_pending: bool,
    /// PPM: the overlay document (a background tab) and the note about unequal page counts.
    pub overlay: Option<(DocId, Option<String>)>,
    /// PPM: the overlay couldn't be built.
    pub overlay_failed: bool,
}

/// The PPM build follows the viewer's compare screen: the current file on the left, the chosen
/// one on the right, words shaded red (only in the current file or changed) and green (only in
/// the chosen file or changed) like the viewer's semantic diff, no visual marks, and an overlay
/// of the pages (current red, chosen blue, same gray).
pub mod ppm {
    use egui::Color32;

    /// The viewer's semantic-diff colours (WebViewer's defaults) and how strongly they are shaded.
    pub const BASE: Color32 = Color32::from_rgb(255, 73, 73);
    pub const TARGET: Color32 = Color32::from_rgb(21, 205, 131);
    pub const MARK_ALPHA: f32 = 0.4;
    /// Height of the strip above the pages: the view buttons, the legend and a note.
    pub const STRIP_H: f32 = 50.0;
    /// Overlay pages are drawn at this dpi.
    pub const OVERLAY_DPI: f32 = 100.0;
    /// Context of the strings below in the catalogs.
    pub const CTX: &str = "compare viewer";

    pub fn rgb(c: [u8; 3]) -> Color32 {
        Color32::from_rgb(c[0], c[1], c[2])
    }
}

/// Visual differences are shaded orange.
pub const VISUAL: Color32 = Color32::from_rgb(0xF0, 0x8C, 0x1A);

/// What the panel asks for.
pub enum PanelAction {
    Select(usize),
    Mark,
    Report,
    Clear,
    SideBySide(bool),
}

pub fn colour(kind: Kind) -> Color32 {
    let [r, g, b] = pdfcraft_engine::compare::colour(kind);
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// The Compare Files dialog: pick the older document. Returns (compare, cancel).
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> (bool, bool) {
    ui.label(egui::RichText::new(tl!("Compare Files")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    let Some((_, new)) = app.active_ids() else { return (false, true) };
    let others: Vec<(DocId, String)> =
        app.views.iter().filter(|v| v.id != new).filter_map(|v| app.session.get(v.id).map(|d| (d.id, d.name.clone()))).collect();
    if app.compare_old.is_none_or(|o| !others.iter().any(|(id, _)| *id == o)) {
        app.compare_old = others.first().map(|(id, _)| *id);
    }
    let new_name = app.session.get(new).map(|d| d.name.clone()).unwrap_or_default();
    egui::Grid::new("compare-files").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label(tl!("Old file:"));
        let shown =
            others.iter().find(|(id, _)| Some(*id) == app.compare_old).map_or(tl!("Open the older version first").to_string(), |(_, n)| n.clone());
        egui::ComboBox::from_id_salt("compare-old").selected_text(shown).width(320.0).show_ui(ui, |ui| {
            for (id, n) in &others {
                ui.selectable_value(&mut app.compare_old, Some(*id), n);
            }
        });
        ui.end_row();
        ui.label(tl!("New file:"));
        ui.label(egui::RichText::new(new_name).strong());
        ui.end_row();
    });
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Text is compared word by word; the differences are shown on the new file.")).small().color(t.text_muted));
    ui.add_space(10.0);
    let (mut go, mut cancel) = (false, false);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add_enabled_ui(app.compare_old.is_some(), |ui| widgets::pill_button(ui, tl!("Compare"), true)).inner.clicked() {
                go = true;
            }
            if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                cancel = true;
            }
        })
    });
    (go, cancel)
}

/// The Compare panel.
pub(crate) fn panel(ui: &mut egui::Ui, t: &Tokens, state: &Option<CompareState>, id: DocId, hosted: bool) -> Option<PanelAction> {
    let Some(s) = state.as_ref().filter(|s| s.new == id) else {
        ui.label(egui::RichText::new(tl!("Compare this file with an older version: Compare files… in All tools.")).color(t.text_muted));
        return None;
    };
    if s.ppm {
        return ppm_panel(ui, t, s);
    }
    let mut action = None;
    let c = &s.result;
    if c.identical() {
        ui.label(tl!("No differences in the text."));
    }
    ui.horizontal_wrapped(|ui| {
        for k in [Kind::Replaced, Kind::Inserted, Kind::Deleted] {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().rect_filled(dot, egui::CornerRadius::same(2), colour(k));
            ui.label(crate::i18n::fmt(tl!("{n} {kind}"), &[("n", &c.count(k).to_string()), ("kind", tl!(k.label()))]));
        }
        let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().rect_filled(dot, egui::CornerRadius::same(2), VISUAL);
        ui.label(crate::i18n::fmt(tl!("{n} {kind}"), &[("n", &s.visual.len().to_string()), ("kind", tl!("Visual"))]));
    });
    ui.add_space(6.0);
    // Side by side, or the new file alone.
    ui.horizontal(|ui| {
        if widgets::pill_button(ui, tl!("Side by side"), s.side_by_side).clicked() {
            action = Some(PanelAction::SideBySide(true));
        }
        if widgets::pill_button(ui, tl!("New file only"), !s.side_by_side).clicked() {
            action = Some(PanelAction::SideBySide(false));
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        // Marks would edit the host's document and a report is a new tab the host can't save.
        if !hosted {
            if ui.button(tl!("Mark as comments")).clicked() {
                action = Some(PanelAction::Mark);
            }
            if ui.button(tl!("Report…")).clicked() {
                action = Some(PanelAction::Report);
            }
        }
        if ui.button(tl!("Clear")).clicked() {
            action = Some(PanelAction::Clear);
        }
    });
    ui.separator();
    for (i, ch) in c.changes.iter().enumerate().take(2000) {
        let text = match ch.kind {
            Kind::Replaced => format!("\"{}\" → \"{}\"", ch.old.text, ch.new.text),
            Kind::Inserted => format!("+ \"{}\"", ch.new.text),
            Kind::Deleted => format!("− \"{}\"", ch.old.text),
        };
        let selected = s.selected == Some(i);
        let r = ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                let (bar, _) = ui.allocate_exact_size(egui::vec2(4.0, 34.0), egui::Sense::hover());
                ui.painter().rect_filled(bar, egui::CornerRadius::same(2), colour(ch.kind));
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(crate::i18n::fmt(
                            tl!("{kind} · page {p}"),
                            &[("kind", tl!(ch.kind.label())), ("p", &(ch.new.page + 1).to_string())],
                        ))
                        .small()
                        .color(t.text_muted),
                    );
                    ui.selectable_label(selected, text)
                })
                .inner
            })
            .inner
        });
        if r.inner.clicked() {
            action = Some(PanelAction::Select(i));
        }
    }
    action
}

/// The PPM panel: the viewer's change list, grouped by page. What only the current file has
/// (the viewer's "base") is a deletion, what only the chosen file has an insertion.
fn ppm_panel(ui: &mut egui::Ui, t: &Tokens, s: &CompareState) -> Option<PanelAction> {
    let c = &s.result;
    let mut action = None;
    ui.label(egui::RichText::new(tl_ctx!(ppm::CTX, "Change list")).font(theme::semibold(14.0)));
    ui.label(crate::i18n::fmt(tl_ctx!(ppm::CTX, "{n} differences"), &[("n", &c.changes.len().to_string())]));
    if c.identical() {
        ui.label(tl!("No differences in the text."));
    }
    ui.separator();
    let words = |text: &str| text.split_whitespace().count().max(1);
    let mut page = None;
    for (i, ch) in c.changes.iter().enumerate().take(2000) {
        if page != Some(ch.new.page) {
            page = Some(ch.new.page);
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(crate::i18n::fmt(tl_ctx!(ppm::CTX, "Page {n}"), &[("n", &(ch.new.page + 1).to_string())]))
                    .small()
                    .color(t.text_muted),
            );
        }
        let (kind, bar) = match ch.kind {
            Kind::Inserted => (tl_ctx!(ppm::CTX, "Delete"), ppm::BASE),
            Kind::Deleted => (tl_ctx!(ppm::CTX, "Insert"), ppm::TARGET),
            Kind::Replaced => (tl_ctx!(ppm::CTX, "Edit"), t.border),
        };
        let fill = if s.selected == Some(i) { t.selected } else { Color32::TRANSPARENT };
        let item = egui::Frame::new().fill(fill).corner_radius(4).inner_margin(4).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (strip, _) = ui.allocate_exact_size(egui::vec2(4.0, 34.0), egui::Sense::hover());
                ui.painter().rect_filled(strip, egui::CornerRadius::same(2), bar);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(tl_ctx!(ppm::CTX, "Change")).small().color(t.text_muted));
                    ui.label(egui::RichText::new(kind).strong());
                    // "Old" is the current file's side and "New" the chosen file's, as in the viewer.
                    if ch.kind != Kind::Deleted {
                        ui.label(egui::RichText::new(format!("{} -{}", tl_ctx!(ppm::CTX, "Old"), words(&ch.new.text))).small().color(t.text_muted));
                        ui.label(&ch.new.text);
                    }
                    if ch.kind != Kind::Inserted {
                        ui.label(egui::RichText::new(format!("{} +{}", tl_ctx!(ppm::CTX, "New"), words(&ch.old.text))).small().color(t.text_muted));
                        ui.label(&ch.old.text);
                    }
                });
            });
        });
        if ui.interact(item.response.rect, ui.id().with(("compare-change", i)), egui::Sense::click()).clicked() {
            action = Some(PanelAction::Select(i));
        }
    }
    action
}

impl PdfCraftApp {
    /// Compare the chosen older document with the active one and show the differences.
    pub fn run_compare(&mut self) {
        let Some((i, new)) = self.active_ids() else { return };
        let Some(old) = self.compare_old else { return };
        match self.session.compare(old, new) {
            Ok(result) => {
                let ppm = self.host_lock;
                // The PPM build shows words only, like the viewer's semantic diff.
                let visual = if ppm { Vec::new() } else { self.session.compare_visual(old, new, 72.0).unwrap_or_default() };
                self.views[i].compare_marks = result
                    .changes
                    .iter()
                    .flat_map(|ch| ch.new.rects.iter().map(move |r| (ch.new.page, r.map(|v| v as f32), if ppm { ppm::BASE } else { colour(ch.kind) })))
                    .collect();
                if ppm {
                    self.views[i].compare_alpha = ppm::MARK_ALPHA;
                }
                // Visual differences not already covered by a text change.
                let text_marks = self.views[i].compare_marks.clone();
                for (p, r) in &visual {
                    let r = r.map(|v| v as f32);
                    let covered = text_marks.iter().any(|(mp, m, _)| *mp == *p && m[0] <= r[2] && r[0] <= m[2] && m[1] <= r[3] && r[1] <= m[3]);
                    if !covered {
                        self.views[i].compare_marks.push((*p, r, VISUAL));
                    }
                }
                // The old file's pages too: what was deleted or replaced, and the visual
                // differences, so both versions read with their own shading.
                if let Some(oi) = self.views.iter().position(|v| v.id == old) {
                    let mut marks: Vec<(usize, [f32; 4], Color32)> = result
                        .changes
                        .iter()
                        .filter(|ch| ch.kind != Kind::Inserted)
                        .flat_map(|ch| ch.old.rects.iter().map(move |r| (ch.old.page, r.map(|v| v as f32), if ppm { ppm::TARGET } else { colour(ch.kind) })))
                        .collect();
                    for (p, r) in &visual {
                        let r = r.map(|v| v as f32);
                        let covered = marks.iter().any(|(mp, m, _)| *mp == *p && m[0] <= r[2] && r[0] <= m[2] && m[1] <= r[3] && r[1] <= m[3]);
                        if !covered {
                            marks.push((*p, r, VISUAL));
                        }
                    }
                    self.views[oi].compare_marks = marks;
                    if ppm {
                        self.views[oi].compare_alpha = ppm::MARK_ALPHA;
                    }
                }
                let n = result.changes.len();
                // Side by side when the old version is open in a tab and the window has the room.
                // The PPM build always does, whatever the width (the viewer has no such limit).
                let side_by_side = self.views.iter().any(|v| v.id == old) && (ppm || self.ctx.as_ref().is_none_or(|c| c.content_rect().width() >= 1000.0));
                self.compare = Some(crate::compare_ui::CompareState {
                    old,
                    new,
                    result,
                    visual,
                    selected: None,
                    side_by_side,
                    leader: 1,
                    prev_scroll: [egui::Vec2::ZERO; 2],
                    ppm,
                    overlay_shown: false,
                    overlay_pending: false,
                    overlay: None,
                    overlay_failed: false,
                });
                self.right = Some(RightPanel::Compare);
                self.notify(if n == 0 {
                    tl!("The text is the same").to_string()
                } else if n == 1 {
                    tl!("1 difference found").to_string()
                } else {
                    crate::i18n::fmt(tl!("{n} differences found"), &[("n", &n.to_string())])
                });
            }
            Err(e) => self.notify_error(e),
        }
    }

    pub(crate) fn compare_action(&mut self, index: usize, a: PanelAction) {
        let Some(s) = self.compare.as_mut() else { return };
        let (old, new) = (s.old, s.new);
        match a {
            PanelAction::Select(i) => {
                s.selected = Some(i);
                let Some(ch) = s.result.changes.get(i) else { return };
                let rect = ch.new.rects.first().copied().or(ch.new.near).unwrap_or_default();
                let page = ch.new.page;
                let old_target = (ch.old.page, ch.old.rects.first().copied().or(ch.old.near).unwrap_or_default());
                // The overlay has the pages only, so it goes to the page.
                let overlay_doc = s.overlay.as_ref().filter(|_| s.overlay_shown).map(|(d, _)| *d);
                let v = &mut self.views[index];
                v.go_to_page(page);
                v.flash = Some((page, rect.map(|x| x as f32), 0.0));
                if let Some(oi) = overlay_doc.and_then(|d| self.views.iter().position(|v| v.id == d)) {
                    self.views[oi].go_to_page(page);
                }
                // The old file's half jumps to its side of the change too.
                if let Some(oi) = self.views.iter().position(|v| v.id == old) {
                    let v = &mut self.views[oi];
                    v.go_to_page(old_target.0);
                    v.flash = Some((old_target.0, old_target.1.map(|x| x as f32), 0.0));
                }
            }
            PanelAction::SideBySide(on) => {
                s.side_by_side = on;
                s.leader = 1;
            }
            PanelAction::Mark => match self.session.mark_differences(old, new) {
                Ok(n) => {
                    if let Some(info) = self.session.get(new).map(|d| d.info.clone()) {
                        self.views[index].document_changed(&info);
                    }
                    self.notify(if n == 1 {
                        tl!("Added 1 comment").to_string()
                    } else {
                        crate::i18n::fmt(tl!("Added {n} comments"), &[("n", &n.to_string())])
                    });
                }
                Err(e) => self.notify_error(e),
            },
            PanelAction::Report => match self.session.compare_report(old, new) {
                Ok(bytes) => {
                    if let Err(e) = self.open_bytes("Compare Report.pdf", None, bytes.to_vec()) {
                        self.notify_error(e);
                    }
                }
                Err(e) => self.notify_error(e),
            },
            PanelAction::Clear => {
                self.compare = None;
                self.views[index].compare_marks.clear();
                if let Some(oi) = self.views.iter().position(|v| v.id == old) {
                    self.views[oi].compare_marks.clear();
                }
                self.right = None;
            }
        }
    }
}

impl PdfCraftApp {
    /// PPM: show the overlay (built on the next frame the first time) or the two halves again.
    pub(crate) fn show_overlay(&mut self, on: bool) {
        let Some(c) = self.compare.as_mut() else { return };
        c.overlay_shown = on;
        if on && c.overlay.is_none() && !c.overlay_failed {
            c.overlay_pending = true;
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint();
            }
        }
    }

    /// PPM: the pages of both files laid over each other (the current file red, the chosen blue,
    /// the same gray) as a PDF in a background tab, built once. The current file is the viewer's
    /// base, so it is the red one.
    fn build_overlay(&mut self) {
        let Some(c) = self.compare.as_mut() else { return };
        c.overlay_pending = false;
        let (old, new) = (c.old, c.new);
        let built = self.session.compare_overlay(new, old, ppm::OVERLAY_DPI).map_err(|e| e.to_string());
        let outcome = built.and_then(|o| {
            // Looked at only: read-only and never synced, like the files it was made from.
            let own = self.session.get(new).map(|d| d.ownership().clone());
            if let Some(own) = own {
                self.session.set_open_ownership(pdfcraft_engine::Ownership { read_only: true, annot_sync: false, hide_comments_panel: false, ..own });
            }
            let before = self.views.len();
            let opened = self.open_bytes_with_focus("compare.pdf", None, o.pdf.to_vec(), false);
            self.session.set_open_ownership(if self.host_lock { crate::host_lock::viewer_ownership() } else { Default::default() });
            opened?;
            if self.views.len() != before + 1 {
                return Err("couldn't open the overlay".to_string());
            }
            let id = self.views[before].id;
            self.session.set_hide_replies(id, true);
            let notice = (o.current_pages != o.chosen_pages).then(|| {
                crate::i18n::fmt(
                    tl_ctx!(ppm::CTX, "Only the first {n} pages were compared (current {a}, chosen {b})."),
                    &[("n", &o.current_pages.min(o.chosen_pages).to_string()), ("a", &o.current_pages.to_string()), ("b", &o.chosen_pages.to_string())],
                )
            });
            Ok((id, notice))
        });
        if let Some(c) = self.compare.as_mut() {
            match outcome {
                Ok(o) => c.overlay = Some(o),
                Err(e) => {
                    c.overlay_failed = true;
                    self.notify_error(e);
                }
            }
        }
    }
}

/// PPM: the strip above the pages, like the viewer's header row and legend. Returns whether
/// the overlay was asked for (`Some(true)`) or the halves (`Some(false)`).
fn ppm_strip(ui: &mut egui::Ui, rect: egui::Rect, t: &Tokens, c: &CompareState) -> Option<bool> {
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(8.0, 4.0))));
    let mut ask = None;
    let busy = c.overlay_shown && c.overlay_pending;
    ui.horizontal_wrapped(|ui| {
        if widgets::pill_button(ui, tl!("Side by side"), !c.overlay_shown).clicked() && c.overlay_shown {
            ask = Some(false);
        }
        if widgets::pill_button(ui, tl_ctx!(ppm::CTX, "Overlay"), c.overlay_shown).clicked() && !c.overlay_shown {
            ask = Some(true);
        }
        if busy {
            ui.label(egui::RichText::new(tl_ctx!(ppm::CTX, "Building the overlay")).small().color(t.text_muted));
        } else if c.overlay_shown {
            for (colour, label) in [
                (ppm::rgb(pdfcraft_engine::compare::OVERLAY_CURRENT), tl_ctx!(ppm::CTX, "Lines only in the current file")),
                (ppm::rgb(pdfcraft_engine::compare::OVERLAY_CHOSEN), tl_ctx!(ppm::CTX, "Lines only in the chosen file")),
                (ppm::rgb(pdfcraft_engine::compare::OVERLAY_SAME), tl_ctx!(ppm::CTX, "Same place")),
            ] {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().rect_filled(dot, egui::CornerRadius::same(2), colour);
                ui.label(egui::RichText::new(label).small());
            }
        } else {
            ui.label(
                egui::RichText::new(tl_ctx!(ppm::CTX, "Red: words only in the current file or changed / Green: words only in the chosen file or changed"))
                    .small()
                    .color(t.text_muted),
            );
        }
    });
    let note = if c.overlay_shown {
        c.overlay.as_ref().and_then(|(_, n)| n.clone())
    } else {
        Some(tl_ctx!(ppm::CTX, "Only words are shown. For pictures and drawings, use Overlay.").to_string())
    };
    if let Some(note) = note {
        ui.label(egui::RichText::new(note).small().color(t.text_muted));
    }
    ask
}

/// The two halves of a side-by-side comparison, as view indexes (old, new), when the active
/// document is the new file of a comparison shown side by side and the old file is open too.
pub(crate) fn side_by_side(app: &PdfCraftApp, index: usize) -> Option<(usize, usize)> {
    let c = app.compare.as_ref().filter(|c| c.side_by_side)?;
    let new = app.views.get(index).filter(|v| v.id == c.new)?;
    let old = app.views.iter().position(|v| v.id == c.old)?;
    (old != index).then_some((old, app.views.iter().position(|v| v.id == new.id)?))
}

/// Side by side: the old file on the left, the new on the right, each a page view of its own
/// with its shading, scrolled and zoomed together. The half the pointer is over leads; the
/// other follows its zoom, layout and scroll offset.
pub(crate) fn compare_area(app: &mut PdfCraftApp, old: usize, new: usize, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let mut rect = ui.available_rect_before_wrap();
    let ppm_on = app.compare.as_ref().is_some_and(|c| c.ppm);
    if ppm_on {
        // The overlay asked for on the last frame is built now, after its status was shown.
        if app.compare.as_ref().is_some_and(|c| c.overlay_pending) {
            app.build_overlay();
        }
        let strip = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), ppm::STRIP_H));
        ui.painter().rect_filled(strip, 0.0, t.panel);
        if let Some(ask) = app.compare.as_ref().and_then(|c| ppm_strip(ui, strip, &t, c)) {
            app.show_overlay(ask);
        }
        rect.min.y += ppm::STRIP_H;
        if let Some(c) = app.compare.as_ref().filter(|c| c.overlay_shown) {
            let body = rect;
            let view = c.overlay.as_ref().and_then(|(d, _)| app.views.iter().position(|v| v.id == *d));
            match view {
                Some(index) => {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("compare-overlay"));
                    child.set_clip_rect(body);
                    crate::canvas::document_area(app, index, &mut child);
                }
                None if c.overlay_failed => {
                    ui.put(body, egui::Label::new(egui::RichText::new(tl_ctx!(ppm::CTX, "Couldn't build the overlay.")).color(t.text_muted)));
                }
                None => {}
            }
            return;
        }
    }
    let gap = 1.0;
    let half_w = ((rect.width() - gap) / 2.0).max(1.0);
    let caption_h = 22.0;
    let left = egui::Rect::from_min_size(rect.min, egui::vec2(half_w, rect.height()));
    let right = egui::Rect::from_min_size(egui::pos2(rect.min.x + half_w + gap, rect.min.y), egui::vec2(half_w, rect.height()));
    // The PPM build puts the current (new) file on the left like the viewer's base; the leader
    // (0 old, 1 new) follows whichever file a half shows.
    let (left_is, right_is) = if ppm_on { (1, 0) } else { (0, 1) };
    // Who leads: the half under the pointer (remembered), the new file at first.
    if let Some(p) = ui.input(|i| i.pointer.hover_pos())
        && let Some(c) = app.compare.as_mut()
    {
        if left.contains(p) {
            c.leader = left_is;
        } else if right.contains(p) {
            c.leader = right_is;
        }
    }
    let leader = app.compare.as_ref().map_or(1, |c| c.leader);
    let (lead, follow) = if leader == 0 { (old, new) } else { (new, old) };
    // The follower takes the leader's zoom and layout before either is drawn.
    let (zoom, fit, layout, continuous, rotation) = {
        let v = &app.views[lead];
        (v.zoom, v.fit, v.layout, v.continuous, v.rotation)
    };
    {
        let v = &mut app.views[follow];
        if v.zoom != zoom || v.fit != fit || v.layout != layout || v.continuous != continuous || v.rotation != rotation {
            v.zoom = zoom;
            v.fit = fit;
            v.layout = layout;
            v.continuous = continuous;
            v.rotation = rotation;
        }
    }
    let shown = if ppm_on { [new, old] } else { [old, new] };
    let names = shown.map(|i| app.session.get(app.views[i].id).map(|d| d.name.clone()).unwrap_or_default());
    ui.painter().vline(rect.min.x + half_w, rect.y_range(), egui::Stroke::new(gap, t.border));
    let halves = if ppm_on { [(left, new), (right, old)] } else { [(left, old), (right, new)] };
    for (k, (r, index)) in halves.into_iter().enumerate() {
        let caption = egui::Rect::from_min_size(r.min, egui::vec2(r.width(), caption_h));
        ui.painter().rect_filled(caption, 0.0, t.panel);
        // The viewer's halves are headed by the file name alone.
        let label = if ppm_on {
            names[k].clone()
        } else {
            crate::i18n::fmt(if k == 0 { tl!("Old file: {name}") } else { tl!("New file: {name}") }, &[("name", &names[k])])
        };
        ui.painter().text(
            egui::pos2(caption.min.x + 10.0, caption.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.0),
            t.text_muted,
        );
        let body = egui::Rect::from_min_max(egui::pos2(r.min.x, r.min.y + caption_h), r.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt(("compare-half", k)));
        child.set_clip_rect(body);
        crate::canvas::document_area(app, index, &mut child);
    }
    // After both drew: the leader's scroll, when it moved, is the follower's next offset.
    let offsets = [app.views[old].last_scroll, app.views[new].last_scroll];
    if let Some(c) = app.compare.as_mut() {
        let lead_k = if leader == 0 { 0 } else { 1 };
        if (offsets[lead_k] - c.prev_scroll[lead_k]).length() > 0.5 && (offsets[1 - lead_k] - offsets[lead_k]).length() > 0.5 {
            app.views[follow].sync_scroll = Some(offsets[lead_k]);
            ui.ctx().request_repaint();
        }
        c.prev_scroll = offsets;
    }
}
