//! Annotation sharing with the embedding page (web `annotSync`, `pdfcraft:annots-*`).
//!
//! The engine does the work (`Session::take_annot_changes`, `annots_reset`…); this module carries
//! the page's messages in, the user's changes and selection out, and keeps the canvas selection
//! pointing at the same annotation when a remote patch moves the indices.

use std::sync::Arc;

use pdfcraft_engine::{AnnotChanges, DocId, RejectedAnnot, RemoteAnnot};

use crate::PdfCraftApp;

/// What the host asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum AnnotCommand {
    Reset { items: Vec<RemoteAnnot> },
    Apply { upserts: Vec<RemoteAnnot>, deletes: Vec<String> },
    Reject { items: Vec<RejectedAnnot> },
    Select { annot_id: String },
}

/// What the host is told.
#[derive(Clone, Debug, PartialEq)]
pub enum AnnotEvent {
    Changed(AnnotChanges),
    Selected(Option<String>),
}

/// Requests from the host, drained every frame once the shared document is open.
pub type AnnotInbox = Arc<std::sync::Mutex<Vec<AnnotCommand>>>;

/// Hands annotation changes and selections to the embedding page (web).
pub type HostAnnotFn = Box<dyn Fn(&AnnotEvent)>;

/// A message's `xfdf` is one annotation; this bounds what a page can make the app parse.
// A stamp's picture (at most 4 MB) travels inside it as base64, a third larger.
const MAX_ITEM_BYTES: usize = 8 << 20;

fn item(v: &serde_json::Value) -> Result<RemoteAnnot, String> {
    let text = |key: &str| v.get(key).and_then(|x| x.as_str()).ok_or_else(|| format!("{key} must be a string"));
    let xfdf = text("xfdf")?;
    if xfdf.len() > MAX_ITEM_BYTES {
        return Err("an annotation is too large".into());
    }
    let (annot_id, author_id) = (text("annotId")?, text("authorId")?);
    if annot_id.is_empty() || author_id.is_empty() {
        return Err("annotId and authorId can't be empty".into());
    }
    Ok(RemoteAnnot { annot_id: annot_id.into(), author_id: author_id.into(), xfdf: xfdf.into() })
}

fn items(v: &serde_json::Value, key: &str) -> Result<Vec<serde_json::Value>, String> {
    v.get(key).and_then(|x| x.as_array()).cloned().ok_or_else(|| format!("{key} must be an array"))
}

/// The items that read well; one that doesn't is left out and logged, so a single bad annotation
/// doesn't keep the rest of the server's list from showing.
fn good<T>(list: Vec<serde_json::Value>, read: impl Fn(&serde_json::Value) -> Result<T, String>) -> Vec<T> {
    list.iter()
        .filter_map(|v| match read(v) {
            Ok(x) => Some(x),
            Err(e) => {
                log::warn!("an annotation from the host was left out: {e}");
                None
            }
        })
        .collect()
}

impl AnnotCommand {
    /// Read a `pdfcraft:annots-*` message (`kind` is the part after `annots-`) from its JSON. A
    /// malformed envelope is an error; a malformed item is skipped.
    pub fn from_json(kind: &str, v: &serde_json::Value) -> Result<Self, String> {
        match kind {
            "reset" => Ok(Self::Reset { items: good(items(v, "items")?, item) }),
            "apply" => {
                let deletes = good(items(v, "deletes")?, |d| d.as_str().map(str::to_string).ok_or_else(|| "a delete must be a string".to_string()));
                Ok(Self::Apply { upserts: good(items(v, "upserts")?, item), deletes })
            }
            "reject" => {
                let one = |r: &serde_json::Value| {
                    let annot_id = r.get("annotId").and_then(|x| x.as_str()).filter(|a| !a.is_empty()).ok_or("annotId must be a string")?;
                    let server = match r.get("server") {
                        None | Some(serde_json::Value::Null) => None,
                        Some(s) => Some(item(s)?),
                    };
                    Ok::<_, String>(RejectedAnnot { annot_id: annot_id.into(), server })
                };
                Ok(Self::Reject { items: good(items(v, "items")?, one) })
            }
            "select" => match v.get("annotId").and_then(|x| x.as_str()) {
                Some(annot_id) => Ok(Self::Select { annot_id: annot_id.into() }),
                None => Err("annotId must be a string".into()),
            },
            other => Err(format!("unknown annotation message {other}")),
        }
    }
}

impl AnnotEvent {
    /// The message body the page gets (`type` is added by the bridge): `annots-changed` or `annots-selected`.
    pub fn to_json(&self) -> (&'static str, serde_json::Value) {
        use serde_json::json;
        match self {
            Self::Changed(c) => (
                "pdfcraft:annots-changed",
                json!({
                    "upserts": c.upserts.iter().map(|u| json!({"annotId": u.annot_id, "page": u.page, "type": u.subtype, "xfdf": u.xfdf})).collect::<Vec<_>>(),
                    "deletes": c.deletes,
                }),
            ),
            Self::Selected(id) => ("pdfcraft:annots-selected", json!({ "annotId": id })),
        }
    }
}

impl PdfCraftApp {
    /// The active document's host asked to hide this interface's comments panel.
    pub(crate) fn comments_panel_hidden(&self) -> bool {
        self.active_ids().and_then(|(_, id)| self.session.get(id)).is_some_and(|d| d.ownership().hide_comments_panel)
    }

    /// A file the host opened is still on its way in: its `annots-*` messages belong to it.
    fn host_open_pending(&self) -> bool {
        self.startup_inbox.lock().map(|q| !q.is_empty()).unwrap_or(false)
    }

    /// Once a frame, after the frame's edits: apply the host's annotation messages, then report
    /// what the user changed and which annotation is selected.
    pub(crate) fn host_annots_tick(&mut self) {
        // The messages wait for the document they belong to (the page sends them right behind
        // `pdfcraft:open`): not for an earlier one, nor while a password is still asked for.
        if self.annot_open_waiting {
            if self.password_prompt.is_some() {
                return;
            }
            self.adopt_annot_target();
        }
        let Some(doc) = self.annot_target.filter(|d| self.session.get(*d).is_some_and(|d| d.annot_sync())) else {
            self.annot_target = None;
            return;
        };
        if self.host_open_pending() {
            return;
        }
        let commands: Vec<AnnotCommand> = self.annot_inbox.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for command in commands {
            self.run_annot_command(doc, command);
        }
        if self.host_annots.is_none() {
            return;
        }
        if let Some(changes) = self.session.take_annot_changes(doc)
            && !changes.is_empty()
            && let Some(report) = self.host_annots.as_ref()
        {
            report(&AnnotEvent::Changed(changes));
        }
        self.report_annot_selection(doc);
    }

    /// The newest shared document becomes the one the messages go to, if it is a new one.
    fn adopt_annot_target(&mut self) {
        let newest = self.session.annot_sync_doc();
        if newest.is_some() && newest != self.annot_target {
            self.annot_target = newest;
            self.annot_open_waiting = false;
            self.annot_selected_sent = None;
        }
    }

    /// A file the host opened with `annotSync` went through the open: its messages follow it, or
    /// (open failed) are dropped, never applied to the document that was open before.
    pub(crate) fn annot_open_finished(&mut self, ok: bool) {
        self.annot_open_waiting = ok;
        if ok {
            self.adopt_annot_target();
        } else {
            self.annot_target = None;
            if let Ok(mut q) = self.annot_inbox.lock() {
                q.clear();
            }
        }
    }

    fn run_annot_command(&mut self, doc: DocId, command: AnnotCommand) {
        // Remote patches shift (page, index): the selection and the open inline edit follow their
        // annotation by `/NM`, or go when it is gone.
        let selected = self.view_of(doc).and_then(|v| v.comments.selected).and_then(|(p, i)| self.annot_name(doc, p, i));
        let editing = self.view_of(doc).and_then(|v| v.comments.editing.as_ref().map(|(p, i, t)| (*p, *i, t.clone())));
        let editing_name = editing.as_ref().and_then(|(p, i, _)| self.annot_name(doc, *p, *i));
        let patched = match command {
            AnnotCommand::Reset { items } => self.session.annots_reset(doc, items),
            AnnotCommand::Apply { upserts, deletes } => self.session.annots_apply(doc, upserts, deletes),
            AnnotCommand::Reject { items } => self.session.annots_reject(doc, items),
            AnnotCommand::Select { annot_id } => {
                self.select_annot(doc, &annot_id);
                return;
            }
        };
        match patched {
            Ok(report) => {
                for (annot_id, why) in &report.failed {
                    log::warn!("annotation {annot_id} from the host was not applied: {why}");
                }
            }
            Err(e) => log::warn!("annotation message refused: {e}"),
        }
        let position = |app: &Self, name: &Option<String>| name.as_deref().and_then(|n| app.session.get(doc)?.annot_position(n));
        let (selected_at, editing_at) = (position(self, &selected), position(self, &editing_name));
        if let Some(view) = self.views.iter_mut().find(|v| v.id == doc) {
            view.comments.selected = selected_at;
            view.comments.editing = match (editing, editing_at) {
                (Some((_, _, text)), Some((p, i))) => Some((p, i, text)),
                _ => None,
            };
            if selected_at.is_none() {
                view.comments.reply.clear();
            }
            // These hold an index, which the patch may have moved to another annotation.
            if matches!(view.comments.gesture, Some(crate::comments::Gesture::Move { .. } | crate::comments::Gesture::Resize { .. })) {
                view.comments.gesture = None;
            }
            if matches!(view.comments.composer.as_ref().map(|c| c.kind), Some(crate::comments::ComposerKind::Edit(_))) {
                view.comments.composer = None;
            }
        }
    }

    fn view_of(&self, doc: DocId) -> Option<&crate::canvas::DocView> {
        self.views.iter().find(|v| v.id == doc)
    }

    fn annot_name(&self, doc: DocId, page: usize, index: usize) -> Option<String> {
        self.session.get(doc)?.annot_name_at(page, index)
    }

    /// `annots-select`: go to the annotation's page and select it. An id this document doesn't have
    /// reports "nothing selected", so the page's card doesn't stay lit.
    fn select_annot(&mut self, doc: DocId, annot_id: &str) {
        let at = self.session.get(doc).and_then(|d| d.annot_position(annot_id));
        let Some(view) = self.views.iter_mut().find(|v| v.id == doc) else { return };
        match at {
            Some((page, index)) => {
                view.go_to_page(page);
                view.comments.selected = Some((page, index));
                view.comments.reveal = true;
            }
            None => view.comments.selected = None,
        }
    }

    /// Tell the page when the canvas selection changed (once per change).
    fn report_annot_selection(&mut self, doc: DocId) {
        let selected = self.view_of(doc).and_then(|v| v.comments.selected).and_then(|(p, i)| self.annot_name(doc, p, i));
        if self.annot_selected_sent.as_ref() == Some(&selected) {
            return;
        }
        if let Some(report) = self.host_annots.as_ref() {
            report(&AnnotEvent::Selected(selected.clone()));
        }
        self.annot_selected_sent = Some(selected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_every_message_shape() {
        let it = json!({"annotId": "a", "authorId": "u", "xfdf": "<x/>"});
        let reset = AnnotCommand::from_json("reset", &json!({"items": [it]})).unwrap();
        assert_eq!(reset, AnnotCommand::Reset { items: vec![RemoteAnnot { annot_id: "a".into(), author_id: "u".into(), xfdf: "<x/>".into() }] });
        let apply = AnnotCommand::from_json("apply", &json!({"upserts": [], "deletes": ["z"]})).unwrap();
        assert_eq!(apply, AnnotCommand::Apply { upserts: vec![], deletes: vec!["z".into()] });
        let reject =
            AnnotCommand::from_json("reject", &json!({"items": [{"annotId": "a", "server": null}, {"annotId": "b", "server": it}]})).unwrap();
        let AnnotCommand::Reject { items } = reject else { panic!("not a reject") };
        assert!(items[0].server.is_none() && items[1].server.is_some());
        assert_eq!(AnnotCommand::from_json("select", &json!({"annotId": "a"})).unwrap(), AnnotCommand::Select { annot_id: "a".into() });
    }

    #[test]
    fn refuses_wrong_shapes() {
        assert!(AnnotCommand::from_json("reset", &json!({})).is_err());
        assert!(AnnotCommand::from_json("reset", &json!({"items": "no"})).is_err());
        assert!(AnnotCommand::from_json("apply", &json!({"upserts": []})).is_err());
        assert!(AnnotCommand::from_json("select", &json!({"annotId": 3})).is_err());
        assert!(AnnotCommand::from_json("nope", &json!({})).is_err());
    }

    #[test]
    fn a_bad_item_is_left_out_and_the_rest_stay() {
        let good = json!({"annotId": "a", "authorId": "u", "xfdf": "<x/>"});
        let bad = json!({"annotId": "b", "xfdf": "<x/>"});
        let AnnotCommand::Reset { items } = AnnotCommand::from_json("reset", &json!({"items": [bad, good, 3]})).unwrap() else {
            panic!("not a reset")
        };
        assert_eq!(items.iter().map(|i| i.annot_id.as_str()).collect::<Vec<_>>(), ["a"]);
        let AnnotCommand::Apply { upserts, deletes } =
            AnnotCommand::from_json("apply", &json!({"upserts": [bad, good], "deletes": [1, "z"]})).unwrap()
        else {
            panic!("not an apply")
        };
        assert_eq!((upserts.len(), deletes), (1, vec!["z".to_string()]));
        let AnnotCommand::Reject { items } =
            AnnotCommand::from_json("reject", &json!({"items": [{"server": null}, {"annotId": "c", "server": bad}, {"annotId": "d"}]})).unwrap()
        else {
            panic!("not a reject")
        };
        assert_eq!(items.iter().map(|i| i.annot_id.as_str()).collect::<Vec<_>>(), ["d"]);
    }

    #[test]
    fn events_become_messages() {
        let (kind, body) = AnnotEvent::Selected(None).to_json();
        assert_eq!((kind, body), ("pdfcraft:annots-selected", json!({"annotId": null})));
        let changes = AnnotChanges {
            upserts: vec![pdfcraft_engine::AnnotUpsert { annot_id: "a".into(), page: 2, subtype: "Text".into(), xfdf: "<x/>".into() }],
            deletes: vec!["d".into()],
        };
        let (kind, body) = AnnotEvent::Changed(changes).to_json();
        assert_eq!(kind, "pdfcraft:annots-changed");
        assert_eq!(body, json!({"upserts": [{"annotId": "a", "page": 2, "type": "Text", "xfdf": "<x/>"}], "deletes": ["d"]}));
    }

    /// One blank page.
    fn pdf() -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>",
            "<< /Type /Page /Parent 2 0 R >>",
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
        out
    }

    fn square(id: &str) -> RemoteAnnot {
        let xfdf = format!(
            r#"<?xml version="1.0" encoding="UTF-8" ?><xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve"><annots><square page="0" name="{id}" rect="10,10,60,60" title="Kim" flags="print"/></annots></xfdf>"#
        );
        RemoteAnnot { annot_id: id.into(), author_id: "u-kim".into(), xfdf }
    }

    fn share() -> pdfcraft_engine::Ownership {
        pdfcraft_engine::Ownership { actor: Some("alice".into()), annot_sync: true, ..Default::default() }
    }

    /// The page's messages go to the document they were sent for: after a second open fails, the
    /// first document is not the target of its list.
    #[test]
    fn messages_of_a_failed_open_are_not_applied_to_the_document_before() {
        let app = PdfCraftApp::new();
        app.ownership_inbox.lock().unwrap().push(share());
        app.startup_inbox.lock().unwrap().push(("a.pdf".into(), pdf()));
        let inbox = app.annot_inbox.clone();
        let mut h = egui_kittest::Harness::builder().build_eframe(move |_| app);
        h.run_steps(4);
        let (_, a) = h.state().active_ids().unwrap();
        inbox.lock().unwrap().push(AnnotCommand::Reset { items: vec![square("first")] });
        h.run_steps(2);
        let at = |h: &egui_kittest::Harness<'_, PdfCraftApp>, nm: &str| h.state().session.get(a).unwrap().annot_position(nm);
        assert!(at(&h, "first").is_some(), "the target receives its list");

        // The second open fails; its list arrives right behind it and must not land on `a`.
        h.state().ownership_inbox.lock().unwrap().push(share());
        h.state().startup_inbox.lock().unwrap().push(("b.pdf".into(), b"not a pdf".to_vec()));
        inbox.lock().unwrap().push(AnnotCommand::Reset { items: vec![square("second")] });
        h.run_steps(4);
        assert!(at(&h, "second").is_none(), "a failed open's list is dropped");
        assert!(at(&h, "first").is_some(), "and the document is as it was");
        assert!(inbox.lock().unwrap().is_empty());
    }

    /// The page posts its list in the same frame as the open: the messages wait for the document
    /// and then apply in the order they came (the delete of the second message hits the first's item).
    #[test]
    fn messages_sent_with_the_open_wait_for_the_document_and_keep_their_order() {
        let app = PdfCraftApp::new();
        app.ownership_inbox.lock().unwrap().push(share());
        app.startup_inbox.lock().unwrap().push(("a.pdf".into(), pdf()));
        {
            let mut q = app.annot_inbox.lock().unwrap();
            q.push(AnnotCommand::Reset { items: vec![square("one"), square("two")] });
            q.push(AnnotCommand::Apply { upserts: vec![square("three")], deletes: vec!["one".into()] });
            q.push(AnnotCommand::Select { annot_id: "three".into() });
        }
        let mut h = egui_kittest::Harness::builder().build_eframe(move |_| app);
        h.run_steps(4);
        let (_, a) = h.state().active_ids().unwrap();
        let doc = h.state().session.get(a).unwrap();
        assert!(doc.annot_position("one").is_none(), "deleted by the second message");
        assert!(doc.annot_position("two").is_some() && doc.annot_position("three").is_some());
        let view = h.state().views.iter().find(|v| v.id == a).unwrap();
        assert_eq!(view.comments.selected, doc.annot_position("three"), "and the third selected it");
        assert!(h.state().annot_inbox.lock().unwrap().is_empty());
    }
}
