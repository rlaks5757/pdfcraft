//! A comparison the embedding page starts (web `pdfcraft:compare`).
//!
//! The host hands in both files: the old one opens in a background tab, the new one becomes the
//! active tab, and the same comparison as Compare Files runs. The host is told the counts.
//! A frame compares once; later requests are answered with "busy".

use std::sync::Arc;

use pdfcraft_engine::compare::Kind;

use crate::PdfCraftApp;

const MAX_ID_CHARS: usize = 128;
const MAX_ERROR_CHARS: usize = 200;

/// The most bytes either file may have; the bridge checks it before copying into memory.
pub const MAX_COMPARE_BYTES: usize = 200 << 20;

/// What the host asks for: `(name, bytes)` of each file, and the rules both open under (its
/// `mode` and `policy`, read like an open's; the narrowest when it names none).
#[derive(Clone, Debug, PartialEq)]
pub struct CompareCommand {
    pub id: String,
    pub old: (String, Vec<u8>),
    pub new: (String, Vec<u8>),
    pub ownership: pdfcraft_engine::Ownership,
}

/// What the host is told.
#[derive(Clone, Debug, PartialEq)]
pub enum CompareEvent {
    Done { id: String, replaced: usize, inserted: usize, deleted: usize, visual: usize },
    Failed { id: String, error: String },
}

/// Requests from the host, drained every frame.
pub type CompareInbox = Arc<std::sync::Mutex<Vec<CompareCommand>>>;

/// Tells the embedding page how the comparison went (web).
pub type HostCompareFn = Box<dyn Fn(&CompareEvent)>;

fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

impl PdfCraftApp {
    fn emit_compare(&self, event: CompareEvent) {
        if let Some(report) = &self.host_compare {
            report(&event);
        }
    }

    fn fail_compare(&self, id: &str, error: &str) {
        self.emit_compare(CompareEvent::Failed { id: cut(id, MAX_ID_CHARS), error: cut(error, MAX_ERROR_CHARS) });
    }

    /// Once a frame: take the host's requests and run the first one.
    pub(crate) fn host_compare_tick(&mut self) {
        let commands: Vec<CompareCommand> = self.compare_inbox.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for command in commands {
            if command.id.is_empty() || command.id.chars().count() > MAX_ID_CHARS {
                self.fail_compare(&command.id, "the request id is missing or too long");
            } else if self.compare_taken {
                self.fail_compare(&command.id, "busy: this window already compares");
            } else {
                self.compare_taken = true;
                match self.start_host_compare(command.old, command.new, command.ownership) {
                    Ok((replaced, inserted, deleted, visual)) => {
                        self.emit_compare(CompareEvent::Done { id: command.id, replaced, inserted, deleted, visual });
                    }
                    Err(error) => self.fail_compare(&command.id, &error),
                }
            }
        }
    }

    /// Open both files (old in the background, new active) and compare them: the counts of
    /// replaced, inserted, deleted and visual differences.
    fn start_host_compare(&mut self, old: (String, Vec<u8>), new: (String, Vec<u8>), ownership: pdfcraft_engine::Ownership) -> Result<(usize, usize, usize, usize), String> {
        // A request supersedes whatever the host armed or queued.
        self.disarm_stamp();
        // The rules come from this request alone, never from an open that came before it.
        // Both files are only looked at: read-only, so nothing here is ever saved back to the parent.
        self.session.set_open_ownership(pdfcraft_engine::Ownership { read_only: true, ..ownership });
        let before = self.views.len();
        let opened = [(old, false), (new, true)].into_iter().try_for_each(|((name, bytes), activate)| {
            let name = if name.trim().is_empty() { "document.pdf".to_string() } else { name };
            self.open_bytes_with_focus(&name, None, bytes, activate).map_err(|e| format!("couldn't open {name}: {e}"))?;
            // An encrypted file asks for a password the host can't give.
            if self.password_prompt.take().is_some() {
                return Err(format!("{name} needs a password"));
            }
            Ok(())
        });
        // Later opens start from the build's own default again.
        self.session.set_open_ownership(if self.host_lock { crate::host_lock::viewer_ownership() } else { Default::default() });
        opened?;
        if self.views.len() != before + 2 {
            return Err("couldn't open both files".into());
        }
        self.compare = None;
        self.compare_old = Some(self.views[before].id);
        self.run_compare();
        let Some(s) = self.compare.as_ref() else { return Err("the comparison failed".into()) };
        Ok((s.result.count(Kind::Replaced), s.result.count(Kind::Inserted), s.result.count(Kind::Deleted), s.visual.len()))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use pdfcraft_engine::Session;

    use super::*;
    use crate::RightPanel;

    fn text_pdf(text: &str) -> Vec<u8> {
        Session::new().create_from_text("t", text).unwrap().to_vec()
    }

    fn command(id: &str) -> CompareCommand {
        CompareCommand {
            id: id.into(),
            old: ("old.pdf".into(), text_pdf("Delivery within five days. Returns accepted.")),
            new: ("new.pdf".into(), text_pdf("Delivery within three days. Returns accepted for a week.")),
            ownership: Default::default(),
        }
    }

    fn app(hosted: bool) -> (Harness<'static, PdfCraftApp>, Rc<RefCell<Vec<CompareEvent>>>) {
        let events: Rc<RefCell<Vec<CompareEvent>>> = Rc::default();
        let sink = events.clone();
        let mut harness = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.host_compare = Some(Box::new(move |e| sink.borrow_mut().push(e.clone())));
            if hosted {
                app.host_save = Some(Box::new(|_, _, _| Ok(())));
            }
            app
        });
        harness.run_steps(3);
        (harness, events)
    }

    fn send(h: &mut Harness<'static, PdfCraftApp>, c: CompareCommand) {
        h.state().compare_inbox.lock().unwrap().push(c);
        h.run_steps(4);
    }

    #[test]
    fn the_host_starts_a_comparison_and_hears_the_counts() {
        let (mut h, events) = app(true);
        send(&mut h, command("c1"));
        assert_eq!(*events.borrow(), vec![CompareEvent::Done { id: "c1".into(), replaced: 2, inserted: 0, deleted: 0, visual: events_visual(&h) }]);
        let state = h.state();
        assert_eq!(state.right, Some(RightPanel::Compare));
        let (_, active) = state.active_ids().unwrap();
        assert_eq!(state.session.get(active).unwrap().name, "new.pdf", "new is the active tab");
        let s = state.compare.as_ref().unwrap();
        assert_eq!(state.session.get(s.old).unwrap().name, "old.pdf");
        assert_eq!(s.new, active);
    }

    fn events_visual(h: &Harness<'static, PdfCraftApp>) -> usize {
        h.state().compare.as_ref().unwrap().visual.len()
    }

    #[test]
    fn a_second_request_is_busy() {
        let (mut h, events) = app(true);
        send(&mut h, command("c1"));
        send(&mut h, command("c2"));
        let events = events.borrow();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[1], CompareEvent::Failed { id, error } if id == "c2" && error.starts_with("busy")), "{events:?}");
        assert_eq!(h.state().views.len(), 2, "no further tabs");
    }

    #[test]
    fn bad_requests_fail_and_a_bad_id_does_not_use_up_the_window() {
        let (mut h, events) = app(true);
        let mut bad = command("c1");
        bad.new.1 = b"not a pdf".to_vec();
        send(&mut h, bad);
        send(&mut h, command(""));
        send(&mut h, command(&"x".repeat(MAX_ID_CHARS + 1)));
        {
            let events = events.borrow();
            assert_eq!(events.len(), 3);
            assert!(events.iter().all(|e| matches!(e, CompareEvent::Failed { .. })), "{events:?}");
            assert!(matches!(&events[2], CompareEvent::Failed { id, .. } if id.chars().count() == MAX_ID_CHARS));
        }
        assert!(h.state().compare.is_none());
        // A request with a good id and a broken file used the window up.
        send(&mut h, command("c4"));
        assert!(matches!(events.borrow().last(), Some(CompareEvent::Failed { error, .. }) if error.starts_with("busy")));
    }

    #[test]
    fn a_bad_id_alone_leaves_the_window_free() {
        let (mut h, events) = app(true);
        send(&mut h, command(""));
        send(&mut h, command("ok"));
        assert!(matches!(events.borrow().as_slice(), [CompareEvent::Failed { .. }, CompareEvent::Done { id, .. }] if id == "ok"));
    }

    /// Both files open under the request's own rules, whatever an earlier open left behind.
    #[test]
    fn both_files_open_under_the_requests_policy_and_not_an_earlier_opens_rules() {
        use pdfcraft_engine::{EditScope, Ownership};
        let (mut h, events) = app(true);
        h.state_mut().host_lock = true;
        // A builder's open came first in this window.
        h.state_mut().session.set_open_ownership(Ownership { scope: EditScope::All, ..Default::default() });
        let mut c = command("c1");
        c.ownership = crate::with_policy(Ownership { scope: EditScope::Viewer, ..Default::default() }, vec![("page.rotate".into(), true)]);
        send(&mut h, c);
        assert!(matches!(events.borrow().as_slice(), [CompareEvent::Done { .. }]), "{:?}", events.borrow());
        let state = h.state();
        let s = state.compare.as_ref().unwrap();
        for id in [s.old, s.new] {
            let own = state.session.get(id).unwrap().ownership().clone();
            assert_eq!(own.scope, EditScope::Viewer);
            assert_eq!(own.overrides, vec![("page.rotate".to_string(), true)]);
            assert!(own.allow.has(pdfcraft_engine::EditClass::Page));
            assert!(own.read_only, "a compared file is only looked at, never saved back");
        }
    }

    #[test]
    fn a_compare_without_rules_is_a_viewers() {
        use pdfcraft_engine::EditScope;
        let (mut h, _) = app(true);
        h.state_mut().host_lock = true;
        let mut c = command("c1");
        c.ownership = crate::with_policy(pdfcraft_engine::Ownership { scope: crate::open_scope(None), ..Default::default() }, Vec::new());
        send(&mut h, c);
        let state = h.state();
        let s = state.compare.as_ref().unwrap();
        // With the lock built in, no mode is the narrowest; without it, nothing is limited.
        let want = if crate::HOST_LOCK { EditScope::Viewer } else { EditScope::All };
        assert_eq!(state.session.get(s.new).unwrap().ownership().scope, want);
    }

    #[test]
    fn hosted_compare_panel_hides_marks_and_report() {
        let (mut h, _) = app(true);
        send(&mut h, command("c1"));
        h.get_by_label("Clear");
        assert!(h.query_by_label("Mark as comments").is_none());
        assert!(h.query_by_label("Report…").is_none());
    }

    /// Without an embedding page; a host-lock build has no such mode (it shows only the notice).
    #[cfg(not(feature = "host-lock"))]
    #[test]
    fn standalone_compare_panel_keeps_marks_and_report() {
        let (mut h, _) = app(false);
        send(&mut h, command("c1"));
        h.get_by_label("Mark as comments");
        h.get_by_label("Report…");
    }
}
