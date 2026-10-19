//! A stamp the embedding page hands in for one placement (web `pdfcraft:stamp-add`).
//!
//! Unlike the stamp library, an armed stamp is never stored in the settings: it carries the host's
//! signed metadata, lives until one click, and is gone when the tool is left.

use std::sync::Arc;

use crate::{PdfCraftApp, QuickTool, stamps_ui::MAX_STAMP_BYTES};

/// Longest side of an armed stamp, in PDF points (a page is at most 14400 pt).
const MAX_SIDE_PT: f64 = 5000.0;
const MAX_META_BYTES: usize = 64 << 10;
const MAX_ID_BYTES: usize = 200;

/// What the host asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum StampCommand {
    Add { id: String, name: String, bytes: Vec<u8>, width: f64, height: f64, meta: String },
    Cancel { id: String },
}

/// What the host is told.
#[derive(Clone, Debug, PartialEq)]
pub enum StampEvent {
    Armed { id: String },
    Placed { id: String, page: usize },
    Cancelled { id: String },
    Failed { id: String, error: String },
}

/// Requests from the host, drained every frame.
pub type StampInbox = Arc<std::sync::Mutex<Vec<StampCommand>>>;

/// Tells the embedding page how a stamp request went (web).
pub type HostStampFn = Box<dyn Fn(&StampEvent)>;

/// A validated request, waiting for the click that places it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ArmedStamp {
    pub id: String,
    pub name: String,
    pub png: Arc<Vec<u8>>,
    /// Size on the page, in PDF points.
    pub width: f64,
    pub height: f64,
    pub meta: String,
    /// The document that was showing when it was armed; another one cancels it.
    pub doc: Option<pdfcraft_engine::DocId>,
}

/// Check a request before arming it: a readable PNG, a sane size, bounded text.
pub(crate) fn validate(id: &str, name: &str, bytes: Vec<u8>, width: f64, height: f64, meta: &str) -> Result<ArmedStamp, String> {
    if id.is_empty() || id.len() > MAX_ID_BYTES {
        return Err("the request id is missing or too long".into());
    }
    if bytes.len() > MAX_STAMP_BYTES {
        return Err(format!("the stamp is too large (at most {} MB)", MAX_STAMP_BYTES >> 20));
    }
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 && width <= MAX_SIDE_PT && height <= MAX_SIDE_PT) {
        return Err(format!("the stamp size must be between 0 and {MAX_SIDE_PT} pt"));
    }
    if meta.len() > MAX_META_BYTES {
        return Err("the stamp metadata is too large".into());
    }
    // Reads the header only; the engine decodes the picture when it is placed.
    png::Decoder::new(std::io::Cursor::new(&bytes)).read_info().map_err(|e| format!("not a readable PNG: {e}"))?;
    let name = if name.trim().is_empty() { "Stamp".to_string() } else { name.trim().chars().take(200).collect() };
    Ok(ArmedStamp { id: id.into(), name, png: Arc::new(bytes), width, height, meta: meta.into(), doc: None })
}

impl PdfCraftApp {
    fn emit_stamp(&self, event: StampEvent) {
        if let Some(report) = &self.host_stamp {
            report(&event);
        }
    }

    /// Drop the armed stamp (if any), leave its tool and tell the host.
    pub(crate) fn disarm_stamp(&mut self) {
        if let Some(armed) = self.armed_stamp.take() {
            if self.quick_tool == QuickTool::HostStamp {
                self.quick_tool = QuickTool::Select;
            }
            self.emit_stamp(StampEvent::Cancelled { id: armed.id });
        }
    }

    /// Once a frame: take the host's requests, report placements, and notice when the user left.
    pub(crate) fn host_stamp_tick(&mut self, ctx: &egui::Context) {
        let commands: Vec<StampCommand> = self.stamp_inbox.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for command in commands {
            match command {
                StampCommand::Add { id, name, bytes, width, height, meta } => {
                    // A new request replaces the one still waiting.
                    self.disarm_stamp();
                    let doc = self.active_ids().map(|(_, d)| d);
                    let armed = match (doc, validate(&id, &name, bytes, width, height, &meta)) {
                        (None, _) => Err("no document is open".to_string()),
                        (_, Err(e)) => Err(e),
                        (doc, Ok(a)) => Ok(ArmedStamp { doc, ..a }),
                    };
                    match armed {
                        Ok(a) => {
                            self.armed_stamp = Some(a);
                            self.quick_tool = QuickTool::HostStamp;
                            self.emit_stamp(StampEvent::Armed { id });
                        }
                        Err(error) => self.emit_stamp(StampEvent::Failed { id, error }),
                    }
                }
                StampCommand::Cancel { id } => {
                    if self.armed_stamp.as_ref().is_some_and(|a| a.id == id) {
                        self.disarm_stamp();
                    }
                }
            }
        }
        // A click placed it: the canvas noted the page and went back to selecting.
        let placed = self.views.iter_mut().find_map(|v| v.host_stamp_page.take());
        if let Some(page) = placed
            && let Some(armed) = self.armed_stamp.take()
        {
            self.emit_stamp(StampEvent::Placed { id: armed.id, page });
        }
        let Some(armed) = self.armed_stamp.as_ref() else { return };
        if self.dialog.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.quick_tool = QuickTool::Select;
        }
        // Escape, another tool, another tab or another document all end it.
        let doc = self.active_ids().map(|(_, d)| d);
        if self.quick_tool != QuickTool::HostStamp || doc != armed.doc {
            self.disarm_stamp();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, 2, 2);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[255u8; 16]).unwrap();
        drop(w);
        out
    }

    #[test]
    fn a_good_request_is_armed_with_its_point_size() {
        let a = validate("r1", " Approved ", png_bytes(), 120.0, 40.5, "{\"sig\":\"x\"}").unwrap();
        assert_eq!((a.id.as_str(), a.name.as_str(), a.width, a.height, a.meta.as_str()), ("r1", "Approved", 120.0, 40.5, "{\"sig\":\"x\"}"));
    }

    #[test]
    fn bad_requests_are_refused() {
        let good = png_bytes();
        assert!(validate("", "n", good.clone(), 10.0, 10.0, "{}").is_err(), "empty id");
        assert!(validate("r", "n", b"not a png".to_vec(), 10.0, 10.0, "{}").is_err(), "not a PNG");
        assert!(validate("r", "n", good.clone(), 0.0, 10.0, "{}").is_err(), "zero width");
        assert!(validate("r", "n", good.clone(), f64::NAN, 10.0, "{}").is_err(), "NaN width");
        assert!(validate("r", "n", good.clone(), 10.0, MAX_SIDE_PT + 1.0, "{}").is_err(), "too tall");
        assert!(validate("r", "n", good.clone(), 10.0, 10.0, &"x".repeat(MAX_META_BYTES + 1)).is_err(), "huge meta");
        assert!(validate("r", "n", vec![0; MAX_STAMP_BYTES + 1], 10.0, 10.0, "{}").is_err(), "huge file");
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod flow_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use egui_kittest::Harness;

    use super::*;

    fn pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>",
            "<< /Type /Page /Parent 2 0 R >>",
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    fn png() -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, 2, 2);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[255u8; 16]).unwrap();
        drop(w);
        out
    }

    fn add(id: &str, bytes: Vec<u8>) -> StampCommand {
        StampCommand::Add { id: id.into(), name: "Approved".into(), bytes, width: 90.0, height: 30.0, meta: "{}".into() }
    }

    fn app(with_doc: bool) -> (Harness<'static, PdfCraftApp>, Rc<RefCell<Vec<StampEvent>>>) {
        let events: Rc<RefCell<Vec<StampEvent>>> = Rc::default();
        let sink = events.clone();
        let mut harness = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_eframe(move |_| {
            let mut app = PdfCraftApp::new();
            if with_doc {
                app.open_bytes("a.pdf", None, pdf()).unwrap();
            }
            app.host_stamp = Some(Box::new(move |e| sink.borrow_mut().push(e.clone())));
            app
        });
        harness.run_steps(3);
        (harness, events)
    }

    fn send(h: &mut Harness<'static, PdfCraftApp>, c: StampCommand) {
        h.state().stamp_inbox.lock().unwrap().push(c);
        h.run_steps(2);
    }

    #[test]
    fn arm_replace_cancel_and_place_are_reported() {
        let (mut h, ev) = app(true);
        send(&mut h, add("a", png()));
        assert_eq!(h.state().quick_tool, QuickTool::HostStamp);
        send(&mut h, add("b", png()));
        send(&mut h, StampCommand::Cancel { id: "a".into() });
        assert_eq!(h.state().quick_tool, QuickTool::HostStamp, "cancelling a replaced id changes nothing");
        // The canvas placed it: it notes the page and goes back to selecting.
        h.state_mut().views[0].host_stamp_page = Some(0);
        h.state_mut().quick_tool = QuickTool::Select;
        h.run_steps(2);
        assert!(h.state().armed_stamp.is_none());
        send(&mut h, add("c", png()));
        send(&mut h, StampCommand::Cancel { id: "c".into() });
        assert_eq!(h.state().quick_tool, QuickTool::Select);
        send(&mut h, add("d", png()));
        h.state_mut().quick_tool = QuickTool::Hand;
        h.run_steps(2);
        let want = vec![
            StampEvent::Armed { id: "a".into() },
            StampEvent::Cancelled { id: "a".into() },
            StampEvent::Armed { id: "b".into() },
            StampEvent::Placed { id: "b".into(), page: 0 },
            StampEvent::Armed { id: "c".into() },
            StampEvent::Cancelled { id: "c".into() },
            StampEvent::Armed { id: "d".into() },
            StampEvent::Cancelled { id: "d".into() },
        ];
        assert_eq!(*ev.borrow(), want);
    }

    #[test]
    fn bad_png_and_missing_document_fail_without_arming() {
        let (mut h, ev) = app(true);
        send(&mut h, add("x", b"nope".to_vec()));
        assert!(h.state().armed_stamp.is_none() && h.state().quick_tool != QuickTool::HostStamp);
        assert!(matches!(ev.borrow().as_slice(), [StampEvent::Failed { id, .. }] if id == "x"));
        let (mut h, ev) = app(false);
        send(&mut h, add("y", png()));
        assert!(matches!(ev.borrow().as_slice(), [StampEvent::Failed { id, error }] if id == "y" && error.contains("no document")));
    }
}
