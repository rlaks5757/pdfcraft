//! Annotation sharing with the embedding page (`pdfcraft:open` `annotSync`), docs/alternate/annot-db-contract.md.
//!
//! In this mode the annotations live in the host's database, one row per `/NM`, and the PDF is
//! only the canvas:
//!
//! - **Out.** After every edit, undo and redo the engine compares the actor's own annotations
//!   with what it last handed out (a `/NM` to fingerprint cache) and keeps the difference until
//!   the interface takes it ([`Session::take_annot_changes`]). Doing this in the engine covers
//!   every edit that makes annotations in bulk (stamps, paste, XFDF import, a `Batch`) without
//!   the interface knowing about them.
//! - **In.** The server's annotations are applied outside the edit pipeline
//!   ([`Session::annots_reset`], [`Session::annots_apply`], [`Session::annots_reject`]): no undo
//!   step, no dirty flag, no owner claim, `/PpmOwner` is the server's author. The same change is
//!   written into every stored undo and redo state, so undoing never removes or brings back an
//!   annotation of someone else.
//!
//! Each undo state is a whole document of its own, so a patched-in annotation can have a
//! different object number in each of them. Nothing here compares object numbers: `/IRT` is
//! read as the target's `/NM`, and the fingerprints skip `/P`, `/Parent` and `/Popup`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use pdfcraft_annot::AnnotError;
use pdfcraft_annot::ownership::{is_protected, owner_of};
use pdfcraft_cos::{Dict, ObjRef, Object};

use crate::{DocId, Document, Edit, EditError, Scope, Session};

type Cos = pdfcraft_cos::Document;

/// One of the actor's annotations that is new or different since the last [`AnnotChanges`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotUpsert {
    /// The annotation's `/NM`.
    pub annot_id: String,
    /// 0-based page.
    pub page: usize,
    /// The `/Subtype` ("Highlight", "Stamp", ...).
    pub subtype: String,
    /// The annotation as one XFDF `<annots>` entry (`name` = `/NM`, `page` = page - 1).
    pub xfdf: String,
}

/// What the user's edits did to the actor's annotations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnnotChanges {
    pub upserts: Vec<AnnotUpsert>,
    pub deletes: Vec<String>,
}

impl AnnotChanges {
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.deletes.is_empty()
    }
}

/// An annotation as the server holds it (`pdfcraft:annots-reset` / `-apply` item).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAnnot {
    pub annot_id: String,
    pub author_id: String,
    pub xfdf: String,
}

/// A change of the actor's that the server refused (`pdfcraft:annots-reject` item): `server` is
/// the annotation's state there, `None` when the server has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedAnnot {
    pub annot_id: String,
    pub server: Option<RemoteAnnot>,
}

/// What a remote application did: how many items were applied or skipped as already current, and
/// the ones that could not be applied (annot id, reason).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteReport {
    pub applied: usize,
    pub skipped: usize,
    pub failed: Vec<(String, String)>,
}

/// The XFDF functions this module needs; the one place that names the xfdf crate's API.
mod xf {
    use super::{Cos, RemoteAnnot};

    pub struct Exported {
        pub page: usize,
        pub subtype: String,
        pub xfdf: String,
    }

    pub fn export(doc: &Cos, nm: &str) -> Option<Exported> {
        pdfcraft_xfdf::export_annotation(doc, nm).ok().map(|e| Exported { page: e.page, subtype: e.kind, xfdf: e.xfdf })
    }

    /// Put the annotations in (replacing the ones with the same `/NM`, `/PpmOwner` the item's
    /// author) after removing `deletes`. Nothing here checks ownership.
    pub fn apply(doc: &mut Cos, upserts: &[RemoteAnnot], deletes: &[String]) -> pdfcraft_xfdf::RemoteReport {
        let items: Vec<pdfcraft_xfdf::RemoteItem> =
            upserts.iter().map(|a| pdfcraft_xfdf::RemoteItem { annot_id: &a.annot_id, author_id: &a.author_id, xfdf: &a.xfdf }).collect();
        pdfcraft_xfdf::apply_remote(doc, &items, deletes)
    }
}

/// An annotation of the actor's as last handed out.
#[derive(Clone)]
struct Entry {
    page: usize,
    /// Fingerprint of the annotation's object graph; `None` when it reaches too much to hash.
    fp: Option<(u64, u64)>,
    /// Hash of the XFDF that was handed out (or applied from the server).
    xfdf: u64,
    /// The annotation's dictionary as last seen: the same allocation means nothing changed, so
    /// an edit elsewhere costs this annotation no hashing.
    arc: Option<Arc<Object>>,
    /// The server has it (handed out, or applied from the server). An annotation that was in the
    /// file when it opened is not synced: it has never been handed out.
    synced: bool,
}

/// The sync state of one `annotSync` document.
#[derive(Clone, Default)]
pub(crate) struct SyncState {
    /// [`Document::generation`] the cache is current for.
    seen: u64,
    /// The actor's annotations by `/NM`.
    mine: HashMap<String, Entry>,
    /// Annotations of others applied from the server: `/NM` to a hash of (author, XFDF).
    remote: HashMap<String, u64>,
    /// Changes not yet taken by the interface.
    upserts: BTreeMap<String, AnnotUpsert>,
    deletes: BTreeSet<String>,
}

impl SyncState {
    fn queue_upsert(&mut self, u: AnnotUpsert) {
        self.deletes.remove(&u.annot_id);
        self.upserts.insert(u.annot_id.clone(), u);
    }

    fn queue_delete(&mut self, nm: String) {
        self.upserts.remove(&nm);
        self.deletes.insert(nm);
    }

    /// Forget everything pending or cached about `nm`.
    fn forget(&mut self, nm: &str) {
        self.mine.remove(nm);
        self.remote.remove(nm);
        self.upserts.remove(nm);
        self.deletes.remove(nm);
    }
}

fn hash_of<T: Hash + ?Sized>(v: &T) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

// ── reading annotations ─────────────────────────────────────────────────────────────────────

/// Call `f(page, index, object, dictionary allocation, dictionary)` for every annotation.
fn walk(cos: &Cos, mut f: impl FnMut(usize, usize, Option<ObjRef>, Option<&Arc<Object>>, &Dict)) {
    let Ok(pages) = pdfcraft_annot::page_refs(cos) else { return };
    for (page, p) in pages.iter().enumerate() {
        let page_obj = cos.get(*p);
        let Some(list) = page_obj.as_dict().and_then(|d| d.get(b"Annots")).map(|a| cos.resolve(a)) else { continue };
        let Some(list) = list.as_array() else { continue };
        for (index, entry) in list.iter().enumerate() {
            match entry.as_ref() {
                Some(r) => {
                    let arc = cos.get(r);
                    if let Some(d) = arc.as_dict() {
                        f(page, index, Some(r), Some(&arc), d);
                    }
                }
                None => {
                    if let Some(d) = entry.as_dict() {
                        f(page, index, None, None, d);
                    }
                }
            }
        }
    }
}

fn nm_of(cos: &Cos, d: &Dict) -> Option<String> {
    let o = cos.resolve(d.get(b"NM")?);
    let s = o.as_string()?.to_text();
    (!s.is_empty()).then_some(s)
}

/// The `/NM` of the annotation `d` replies to.
fn parent_nm(cos: &Cos, d: &Dict) -> Option<String> {
    let target = cos.get(d.reference(b"IRT")?);
    nm_of(cos, target.as_dict()?)
}

fn is_mine(cos: &Cos, d: &Dict, actor: &str) -> bool {
    is_protected(d) && owner_of(cos, d).as_deref() == Some(actor)
}

/// Most objects an annotation may reach before it is no longer fingerprinted.
const MAX_OBJECTS: usize = 20_000;
const MAX_DEPTH: usize = 100;

/// A fingerprint of an annotation and everything it points to, or `None` when it reaches too
/// much. Object numbers do not enter: references are numbered by first visit, `/IRT` is the
/// target's `/NM`, and `/P`, `/Parent` and `/Popup` (which lead back out to other annotations
/// and the page) are left out.
fn fingerprint(cos: &Cos, d: &Dict) -> Option<(u64, u64)> {
    struct Walk<'a> {
        cos: &'a Cos,
        seen: HashMap<ObjRef, usize>,
        h: (DefaultHasher, DefaultHasher),
    }
    impl Walk<'_> {
        fn put<T: Hash>(&mut self, v: T) {
            v.hash(&mut self.h.0);
            0xA5u8.hash(&mut self.h.1);
            v.hash(&mut self.h.1);
        }
        fn object(&mut self, o: &Object, depth: usize) -> Option<()> {
            if depth > MAX_DEPTH {
                return None;
            }
            match o {
                Object::Null => self.put(0u8),
                Object::Bool(b) => self.put((1u8, *b)),
                Object::Int(i) => self.put((2u8, *i)),
                Object::Real(r) => self.put((3u8, r.to_bits())),
                Object::String(s) => self.put((4u8, &s.bytes)),
                Object::Name(n) => self.put((5u8, n)),
                Object::Array(items) => {
                    self.put((6u8, items.len()));
                    for i in items {
                        self.object(i, depth + 1)?;
                    }
                }
                Object::Dict(d) => self.dict(d, depth)?,
                Object::Stream(st) => {
                    self.put(7u8);
                    self.dict(&st.dict, depth)?;
                    self.put(&st.raw[..]);
                }
                Object::Ref(r) => {
                    if let Some(n) = self.seen.get(r) {
                        self.put((8u8, *n));
                    } else {
                        if self.seen.len() >= MAX_OBJECTS {
                            return None;
                        }
                        self.seen.insert(*r, self.seen.len());
                        self.put(9u8);
                        let target = self.cos.get(*r);
                        self.object(&target, depth + 1)?;
                    }
                }
            }
            Some(())
        }
        fn dict(&mut self, d: &Dict, depth: usize) -> Option<()> {
            let mut entries: Vec<_> = d.iter().filter(|(k, _)| !matches!(k.as_slice(), b"P" | b"Parent" | b"Popup")).collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            self.put((10u8, entries.len()));
            for (k, v) in entries {
                self.put(k);
                if k.as_slice() == b"IRT" {
                    let target = v.as_ref().and_then(|r| self.cos.get(r).as_dict().and_then(|t| nm_of(self.cos, t)));
                    self.put((11u8, target));
                } else {
                    self.object(v, depth + 1)?;
                }
            }
            Some(())
        }
    }
    let mut w = Walk { cos, seen: HashMap::new(), h: (DefaultHasher::new(), DefaultHasher::new()) };
    w.dict(d, 0)?;
    Some((w.h.0.finish(), w.h.1.finish()))
}

// ── out: the actor's changes ────────────────────────────────────────────────────────────────

fn same_alloc(a: &Option<Arc<Object>>, b: Option<&Arc<Object>>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if Arc::ptr_eq(x, y))
}

/// Compare the actor's annotations in `cos` with the cache, queue what differs, and bring the
/// cache up to date.
fn diff(cos: &Cos, actor: &str, st: &mut SyncState) {
    let mut present: HashSet<String> = HashSet::new();
    let mut found: Vec<(String, usize, Option<Arc<Object>>, Dict)> = Vec::new();
    walk(cos, |page, _, _, arc, d| {
        if !is_mine(cos, d, actor) {
            return;
        }
        let Some(nm) = nm_of(cos, d) else { return };
        // A name used twice: the first is the one that counts.
        if present.insert(nm.clone()) {
            found.push((nm, page, arc.cloned(), d.clone()));
        }
    });
    for (nm, page, arc, d) in found {
        let prev = st.mine.get(&nm);
        if let Some(p) = prev
            && p.page == page
            && same_alloc(&p.arc, arc.as_ref())
        {
            continue;
        }
        let fp = fingerprint(cos, &d);
        if let Some(p) = prev
            && p.page == page
            && fp.is_some()
            && p.fp == fp
        {
            if let Some(e) = st.mine.get_mut(&nm) {
                e.arc = arc;
            }
            continue;
        }
        // An annotation that cannot be written out stays as it was (and is tried again at the
        // next edit), rather than being reported as gone.
        let Some(x) = xf::export(cos, &nm) else { continue };
        let xfdf = hash_of(&x.xfdf);
        let unchanged = prev.is_some_and(|p| p.xfdf == xfdf && p.page == x.page);
        let synced = prev.is_some_and(|p| p.synced);
        st.mine.insert(nm.clone(), Entry { page: x.page, fp, xfdf, arc, synced: synced || !unchanged });
        if !unchanged {
            st.queue_upsert(AnnotUpsert { annot_id: nm, page: x.page, subtype: x.subtype, xfdf: x.xfdf });
        }
    }
    let gone: Vec<String> = st.mine.keys().filter(|k| !present.contains(*k)).cloned().collect();
    for nm in gone {
        let was_synced = st.mine.remove(&nm).is_some_and(|e| e.synced);
        // Never handed out: the server knows nothing of it.
        if was_synced {
            st.queue_delete(nm);
        } else {
            st.upserts.remove(&nm);
        }
    }
}

/// Bring the cache up to date with the document's current state.
fn sync_now(doc: &mut Document) {
    let (Some(st), Some(actor), Some(editor)) = (doc.sync.as_mut(), doc.ownership.actor.as_deref(), doc.editor.as_ref()) else { return };
    if st.seen == doc.generation {
        return;
    }
    diff(&editor.cos, actor, st);
    st.seen = doc.generation;
}

impl Document {
    /// Start tracking the actor's annotations from the document as it is (none of them are
    /// handed out), or stop. Called when the ownership of a document is set.
    pub(crate) fn init_annot_sync(&mut self) {
        let on = self.ownership.annot_sync && self.ownership.actor.is_some() && self.editor.is_some();
        if !on {
            self.sync = None;
            return;
        }
        if self.sync.is_some() {
            return;
        }
        let mut st = SyncState { seen: self.generation, ..Default::default() };
        if let (Some(actor), Some(editor)) = (self.ownership.actor.as_deref(), self.editor.as_ref()) {
            diff(&editor.cos, actor, &mut st);
        }
        // What was in the file is not news.
        st.upserts.clear();
        st.deletes.clear();
        for e in st.mine.values_mut() {
            e.synced = false;
        }
        self.sync = Some(st);
    }

    /// Does this document exchange its annotations with the host?
    pub fn annot_sync(&self) -> bool {
        self.sync.is_some()
    }

    /// Page and index (in `/Annots`, both 0-based) of the annotation named `nm`.
    pub fn annot_position(&self, nm: &str) -> Option<(usize, usize)> {
        let editor = self.editor.as_ref()?;
        let mut at = None;
        walk(&editor.cos, |page, index, _, _, d| {
            if at.is_none() && nm_of(&editor.cos, d).as_deref() == Some(nm) {
                at = Some((page, index));
            }
        });
        at
    }

    /// The `/NM` of the annotation at `(page, index)`.
    pub fn annot_name_at(&self, page: usize, index: usize) -> Option<String> {
        let editor = self.editor.as_ref()?;
        let mut nm = None;
        walk(&editor.cos, |p, i, _, _, d| {
            if p == page && i == index {
                nm = nm_of(&editor.cos, d);
            }
        });
        nm
    }

    /// Is the annotation at `(page, index)` a reply to another annotation?
    pub fn annot_is_reply(&self, page: usize, index: usize) -> bool {
        self.editor.as_ref().is_some_and(|e| is_reply_at(&e.cos, page, index))
    }
}

/// Is the annotation at `(page, index)` of `cos` a reply to another?
pub(crate) fn is_reply_at(cos: &Cos, page: usize, index: usize) -> bool {
    let mut reply = false;
    walk(cos, |p, i, _, _, d| {
        if p == page && i == index {
            reply = d.get(b"IRT").is_some();
        }
    });
    reply
}

/// Replies to replies are refused (the server does): the edit says which annotation it replies
/// to or sets the state of.
pub(crate) fn check_reply_target(cos: &Cos, edit: &Edit) -> Result<(), EditError> {
    if let Edit::ReplyToAnnotation { page, index, .. } | Edit::SetAnnotationStatus { page, index, .. } | Edit::MarkAnnotation { page, index, .. } =
        edit
        && is_reply_at(cos, *page, *index)
    {
        return Err(EditError::Invalid("a reply can't be replied to".into()));
    }
    Ok(())
}

/// Undo or redo to `target` must not remove an annotation of the actor's that someone else has
/// replied to in the current state; the server would refuse the delete.
fn check_step(current: &Cos, target: &Cos, actor: &str) -> Result<(), AnnotError> {
    let mut parents: HashSet<String> = HashSet::new();
    let mut mine: Vec<String> = Vec::new();
    walk(current, |_, _, _, _, d| {
        if !is_protected(d) {
            return;
        }
        if owner_of(current, d).as_deref() == Some(actor) {
            mine.extend(nm_of(current, d));
        } else if let Some(p) = parent_nm(current, d) {
            parents.insert(p);
        }
    });
    mine.retain(|nm| parents.contains(nm));
    if mine.is_empty() {
        return Ok(());
    }
    let mut kept: HashSet<String> = HashSet::new();
    walk(target, |_, _, _, _, d| kept.extend(nm_of(target, d)));
    if mine.iter().any(|nm| !kept.contains(nm)) {
        return Err(AnnotError::ForeignReplies);
    }
    Ok(())
}

// ── in: the server's annotations ────────────────────────────────────────────────────────────

impl Session {
    fn sync_doc_mut(&mut self, id: DocId) -> Result<&mut Document, EditError> {
        let doc = self.doc_mut(id)?;
        if doc.sync.is_none() {
            return Err(EditError::Invalid("the page didn't ask for annotation sharing (annotSync) when it opened this document".into()));
        }
        Ok(doc)
    }

    /// The newest open document that exchanges annotations with the host.
    pub fn annot_sync_doc(&self) -> Option<DocId> {
        self.docs.iter().rev().find(|d| d.sync.is_some()).map(|d| d.id)
    }

    /// What the user's edits changed in the actor's annotations since this was last called.
    /// `None` when there is nothing, or the document doesn't share annotations.
    pub fn take_annot_changes(&mut self, id: DocId) -> Option<AnnotChanges> {
        let doc = self.docs.iter_mut().find(|d| d.id == id)?;
        sync_now(doc);
        let st = doc.sync.as_mut()?;
        if st.upserts.is_empty() && st.deletes.is_empty() {
            return None;
        }
        let mut changes = AnnotChanges {
            upserts: std::mem::take(&mut st.upserts).into_values().collect(),
            deletes: std::mem::take(&mut st.deletes).into_iter().collect(),
        };
        changes.upserts.sort_by(|a, b| (a.page, &a.annot_id).cmp(&(b.page, &b.annot_id)));
        // Everything handed out is now known to the server.
        for u in &changes.upserts {
            if let Some(e) = st.mine.get_mut(&u.annot_id) {
                e.synced = true;
            }
        }
        Some(changes)
    }

    /// The server's whole list: annotations of the server's that aren't in it are removed, the
    /// rest are put in (an annotation that is already here keeps its place in the document, so
    /// replies stay linked). Annotations that came with the file are left alone.
    pub fn annots_reset(&mut self, id: DocId, items: Vec<RemoteAnnot>) -> Result<RemoteReport, EditError> {
        let doc = self.sync_doc_mut(id)?;
        sync_now(doc);
        let Some(st) = doc.sync.as_ref() else { return Ok(RemoteReport::default()) };
        let listed: HashSet<&str> = items.iter().map(|i| i.annot_id.as_str()).collect();
        // Not the ones made since the last hand-out: they are not on the server yet.
        let deletes: Vec<String> = st
            .remote
            .keys()
            .chain(st.mine.iter().filter(|(nm, e)| e.synced && !st.upserts.contains_key(*nm)).map(|(nm, _)| nm))
            .filter(|nm| !listed.contains(nm.as_str()))
            .cloned()
            .collect();
        Self::apply_remote(doc, items, deletes, true)
    }

    /// Changes the server pushed: annotations to put in (the same `/NM` is replaced) and `/NM`s to remove.
    pub fn annots_apply(&mut self, id: DocId, upserts: Vec<RemoteAnnot>, deletes: Vec<String>) -> Result<RemoteReport, EditError> {
        let doc = self.sync_doc_mut(id)?;
        Self::apply_remote(doc, upserts, deletes, true)
    }

    /// The server refused these changes of the actor's: put each annotation back as the server has it.
    pub fn annots_reject(&mut self, id: DocId, items: Vec<RejectedAnnot>) -> Result<RemoteReport, EditError> {
        let doc = self.sync_doc_mut(id)?;
        let mut upserts = Vec::new();
        let mut deletes = Vec::new();
        for i in items {
            match i.server {
                Some(mut s) => {
                    s.annot_id = i.annot_id;
                    upserts.push(s);
                }
                None => deletes.push(i.annot_id),
            }
        }
        // Always applied: the local copy is what differs from the server.
        Self::apply_remote(doc, upserts, deletes, false)
    }

    /// Apply the server's changes outside the edit pipeline: to the working document and to every
    /// state undo and redo can bring back. `skip_known` leaves out an annotation that is already
    /// here exactly as given.
    fn apply_remote(doc: &mut Document, upserts: Vec<RemoteAnnot>, deletes: Vec<String>, skip_known: bool) -> Result<RemoteReport, EditError> {
        // Changes the user made but the interface hasn't taken yet stay pending.
        sync_now(doc);
        let mut report = RemoteReport::default();
        let actor = doc.ownership.actor.clone().unwrap_or_default();
        let hash_item = |a: &RemoteAnnot| hash_of(&(&a.author_id, &a.xfdf));
        let (live, known): (Vec<RemoteAnnot>, Vec<RemoteAnnot>) =
            upserts.into_iter().partition(|a| !(skip_known && doc.sync.as_ref().and_then(|s| s.remote.get(&a.annot_id)) == Some(&hash_item(a))));
        report.skipped = known.len();
        let reason = doc.read_only_reason.clone().unwrap_or_default();
        let editor = doc.editor.as_mut().ok_or(EditError::ReadOnly(reason))?;
        let done = xf::apply(&mut editor.cos, &live, &deletes);
        report.failed = done.failed.clone();
        report.applied = done.applied.len() + done.removed.len();
        // The stored states get what the working document got; one that can't take an item is a
        // state undo then shows without it.
        let placed: HashSet<&str> = done.applied.iter().map(String::as_str).collect();
        let on_states: Vec<RemoteAnnot> = live.iter().filter(|a| placed.contains(a.annot_id.as_str())).cloned().collect();
        if !on_states.is_empty() || !deletes.is_empty() {
            for (_, state, _, _) in editor.undo.iter_mut().chain(editor.redo.iter_mut()) {
                xf::apply(state, &on_states, &deletes);
            }
        }
        if done.applied.is_empty() && done.removed.is_empty() {
            return Ok(report);
        }
        let touched: HashSet<String> = done.applied.iter().chain(&deletes).cloned().collect();
        let hashes: HashMap<&str, u64> =
            live.iter().filter(|a| placed.contains(a.annot_id.as_str())).map(|a| (a.annot_id.as_str(), hash_item(a))).collect();
        // Same refresh as a comment edit, but no undo step, no dirty flag.
        let refreshed = Session::refresh_scoped(doc, Scope::Comments);
        doc.generation += 1;
        if let Err(e) = refreshed {
            Session::show_released(doc);
            return Err(e);
        }
        // What the server sent is not the user's change: take it into the cache so it isn't handed back.
        let generation = doc.generation;
        let (Some(st), Some(editor)) = (doc.sync.as_mut(), doc.editor.as_ref()) else { return Ok(report) };
        for nm in &touched {
            st.forget(nm);
        }
        let cos = &editor.cos;
        let mut seen: HashSet<String> = HashSet::new();
        type Touched = (String, Option<Arc<Object>>, usize, Option<(u64, u64)>, bool);
        let mut entries: Vec<Touched> = Vec::new();
        walk(cos, |page, _, _, arc, d| {
            let Some(nm) = nm_of(cos, d) else { return };
            if !touched.contains(&nm) || !seen.insert(nm.clone()) {
                return;
            }
            entries.push((nm, arc.cloned(), page, fingerprint(cos, d), is_mine(cos, d, &actor)));
        });
        for (nm, arc, page, fp, mine) in entries {
            if mine {
                let xfdf = xf::export(cos, &nm).map_or(0, |x| hash_of(&x.xfdf));
                st.mine.insert(nm, Entry { page, fp, xfdf, arc, synced: true });
            } else if let Some(h) = hashes.get(nm.as_str()) {
                st.remote.insert(nm, *h);
            }
        }
        st.seen = generation;
        Ok(report)
    }

    /// Refuse an undo or redo that would take away an annotation someone replied to.
    pub(crate) fn check_history_step(doc: &Document, undo: bool) -> Result<(), EditError> {
        let (Some(actor), Some(editor)) = (doc.ownership.actor.as_deref(), doc.editor.as_ref()) else { return Ok(()) };
        if doc.sync.is_none() {
            return Ok(());
        }
        let step = if undo { editor.undo.last() } else { editor.redo.last() };
        let Some((_, target, _, _)) = step else { return Ok(()) };
        Ok(check_step(&editor.cos, target, actor)?)
    }
}

/// Is `edit` work on annotations only, which in `annotSync` mode goes to the host's database and
/// so is nothing to save?
pub(crate) fn is_annotation_work(edit: &Edit) -> bool {
    match edit {
        Edit::Batch { edits, .. } => edits.iter().all(is_annotation_work),
        Edit::ImportData { .. } => true,
        other => crate::scope_of(other) == Scope::Comments,
    }
}
