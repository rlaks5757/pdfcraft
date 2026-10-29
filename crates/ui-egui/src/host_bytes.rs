//! The embedding page asking for the document's bytes (`pdfcraft:request-bytes`): the parent's
//! own Save (Ctrl/Cmd+S pressed outside the frame) takes the file as it is now, without the
//! user pressing Save in here. Answered with `pdfcraft:bytes`, or `pdfcraft:bytes-failed`.

use std::sync::Arc;

use pdfcraft_engine::{DocId, EditScope};

use crate::PdfCraftApp;

/// Why the page wants the bytes. `Save` stores the file (it counts as saved, as after a Save in
/// here); `Check` and `Preview` change nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BytesPurpose {
    Save,
    Check,
    Preview,
}

impl BytesPurpose {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "save" => Some(Self::Save),
            "check" => Some(Self::Check),
            "preview" => Some(Self::Preview),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::Check => "check",
            Self::Preview => "preview",
        }
    }
}

/// A request of the page, its `id` already checked by the web app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BytesRequest {
    pub id: String,
    pub purpose: BytesPurpose,
}

/// Requests from the host, drained every frame.
pub type BytesInbox = Arc<std::sync::Mutex<Vec<BytesRequest>>>;

/// The answer to one request.
#[derive(Clone, Debug)]
pub enum BytesReply {
    /// `pages_changed` is only ever `true` for a builder's document (see `Session::pages_changed`).
    Bytes { id: String, purpose: BytesPurpose, name: String, bytes: Arc<Vec<u8>>, pages_changed: bool },
    /// `reason`: `not-ready` (no document is open), `edit-pending` or an error text.
    Failed { id: String, purpose: BytesPurpose, reason: String },
}

/// What a `Save` request marks as saved once its reply is delivered.
type SavedDoc = Option<(DocId, Arc<Vec<u8>>)>;

/// Hands one answer to the embedding page (web); an error says it could not be delivered.
pub type HostBytesFn = Box<dyn Fn(&BytesReply) -> Result<(), String>>;

impl PdfCraftApp {
    /// Once a frame: answer the host's byte requests with the active document.
    pub(crate) fn host_bytes_tick(&mut self) {
        let requests: Vec<BytesRequest> = self.bytes_inbox.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for request in requests {
            let (reply, saved) = self.bytes_reply(&request);
            let delivered = self.host_bytes.as_ref().map(|send| send(&reply));
            match (delivered, saved) {
                // Handed over for storing: the document counts as saved.
                (Some(Ok(())), Some((id, bytes))) => {
                    let _ = self.session.mark_saved(id, bytes, None);
                    if let Some(doc) = self.session.get(id)
                        && let Some(view) = self.views.iter_mut().find(|v| v.id == id)
                    {
                        view.document_changed(&doc.info);
                    }
                }
                (Some(Err(e)), _) => log::error!("couldn't answer the bytes request {}: {e}", request.id),
                _ => {}
            }
        }
    }

    /// The reply, and for `Save` the document and bytes to mark saved once the reply is delivered.
    fn bytes_reply(&mut self, request: &BytesRequest) -> (BytesReply, SavedDoc) {
        let (id, purpose) = (request.id.clone(), request.purpose);
        let failed = |reason: &str| (BytesReply::Failed { id: id.clone(), purpose, reason: reason.to_string() }, None);
        let Some((_, doc)) = self.active_ids() else { return failed("not-ready") };
        if purpose == BytesPurpose::Save {
            // What is typed in a field is saved with the document, as for Save in here.
            if !self.commit_typing_in(doc) {
                return failed("edit-pending");
            }
            if !self.flatten_fill_sign_for_save(doc) {
                return failed("the document can't be changed");
            }
        }
        let bytes = match self.session.save_bytes(doc) {
            Ok(b) => b,
            Err(e) => return failed(&e.to_string()),
        };
        let Some(d) = self.session.get(doc) else { return failed("not-ready") };
        let pages_changed = d.ownership().scope == EditScope::All && self.session.pages_changed(doc);
        let reply = BytesReply::Bytes { id: id.clone(), purpose, name: d.name.clone(), bytes: bytes.clone(), pages_changed };
        (reply, (purpose == BytesPurpose::Save).then_some((doc, bytes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use pdfcraft_engine::{Edit, Ownership};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Two blank pages.
    fn pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 200 300] >>",
            "<< /Type /Page /Parent 2 0 R >>",
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

    type Replies = Rc<RefCell<Vec<BytesReply>>>;

    fn app(scope: EditScope, with_doc: bool) -> (Harness<'static, PdfCraftApp>, Replies) {
        let replies: Replies = Rc::default();
        let sink = replies.clone();
        let mut harness = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_eframe(move |_| {
            let mut app = PdfCraftApp::new();
            app.session.set_open_ownership(Ownership { scope, ..Default::default() });
            if with_doc {
                app.open_bytes("a.pdf", None, pdf()).unwrap();
            }
            app.host_bytes = Some(Box::new(move |r| {
                sink.borrow_mut().push(r.clone());
                Ok(())
            }));
            // A host-lock build shows nothing and opens nothing until a host takes saves.
            app.host_save = Some(Box::new(|_, _, _, _| Ok(())));
            app
        });
        harness.run_steps(3);
        (harness, replies)
    }

    fn ask(h: &mut Harness<'static, PdfCraftApp>, id: &str, purpose: BytesPurpose) {
        h.state().bytes_inbox.lock().unwrap().push(BytesRequest { id: id.into(), purpose });
        h.run_steps(2);
    }

    #[test]
    fn a_builder_save_request_carries_pages_changed_and_counts_as_saved() {
        let (mut h, replies) = app(EditScope::All, true);
        ask(&mut h, "r1", BytesPurpose::Save);
        assert!(matches!(&replies.borrow()[0], BytesReply::Bytes { id, purpose: BytesPurpose::Save, pages_changed: false, .. } if id == "r1"));
        let (_, doc) = h.state().active_ids().unwrap();
        assert!(h.state_mut().apply_edit(Edit::DeletePages { pages: vec![0] }));
        assert!(h.state().session.get(doc).unwrap().dirty);
        ask(&mut h, "r2", BytesPurpose::Save);
        assert!(matches!(&replies.borrow()[1], BytesReply::Bytes { id, pages_changed: true, .. } if id == "r2"));
        assert!(!h.state().session.get(doc).unwrap().dirty, "a delivered save counts as saved");
        // It stays true after the save: the page's comments still go by the file as opened.
        ask(&mut h, "r3", BytesPurpose::Check);
        assert!(matches!(&replies.borrow()[2], BytesReply::Bytes { purpose: BytesPurpose::Check, pages_changed: true, .. }));
    }

    #[test]
    fn check_changes_nothing_and_no_document_is_not_ready() {
        let (mut h, replies) = app(EditScope::All, true);
        assert!(h.state_mut().apply_edit(Edit::RotatePages { pages: vec![0], degrees: 90 }));
        ask(&mut h, "c", BytesPurpose::Check);
        let (_, doc) = h.state().active_ids().unwrap();
        assert!(h.state().session.get(doc).unwrap().dirty);
        assert!(matches!(&replies.borrow()[0], BytesReply::Bytes { pages_changed: true, .. }));
        let (mut h, replies) = app(EditScope::All, false);
        ask(&mut h, "n", BytesPurpose::Save);
        assert!(matches!(&replies.borrow()[0], BytesReply::Failed { reason, .. } if reason == "not-ready"));
    }

    #[test]
    fn a_viewer_never_reports_pages_changed() {
        let (mut h, replies) = app(EditScope::Viewer, true);
        ask(&mut h, "v", BytesPurpose::Save);
        assert!(matches!(&replies.borrow()[0], BytesReply::Bytes { pages_changed: false, .. }));
    }
}
