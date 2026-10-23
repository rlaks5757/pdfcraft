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
const MAX_ITEM_BYTES: usize = 4 << 20;

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

impl AnnotCommand {
    /// Read a `pdfcraft:annots-*` message (`kind` is the part after `annots-`) from its JSON; the
    /// error says what was wrong with it.
    pub fn from_json(kind: &str, v: &serde_json::Value) -> Result<Self, String> {
        match kind {
            "reset" => Ok(Self::Reset { items: items(v, "items")?.iter().map(item).collect::<Result<_, _>>()? }),
            "apply" => {
                let deletes = items(v, "deletes")?
                    .iter()
                    .map(|d| d.as_str().map(str::to_string).ok_or_else(|| "deletes must be strings".to_string()))
                    .collect::<Result<_, _>>()?;
                Ok(Self::Apply { upserts: items(v, "upserts")?.iter().map(item).collect::<Result<_, _>>()?, deletes })
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
                Ok(Self::Reject { items: items(v, "items")?.iter().map(one).collect::<Result<_, _>>()? })
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
        let Some(doc) = self.session.annot_sync_doc() else {
            // Nothing shared is open yet: the messages wait for the document (the page sends them
            // right behind `pdfcraft:open`).
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
        assert!(AnnotCommand::from_json("reset", &json!({"items": [{"annotId": "a", "xfdf": "<x/>"}]})).is_err());
        assert!(AnnotCommand::from_json("apply", &json!({"upserts": [], "deletes": [1]})).is_err());
        assert!(AnnotCommand::from_json("reject", &json!({"items": [{"server": null}]})).is_err());
        assert!(AnnotCommand::from_json("select", &json!({"annotId": 3})).is_err());
        assert!(AnnotCommand::from_json("nope", &json!({})).is_err());
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
}
