//! One annotation at a time, for the host's annotation database (PPM server).
//!
//! The server keeps each annotation as one XFDF document with exactly one element inside
//! `<annots>`, its `name` equal to the annotation id (our `/NM`) and a zero-based `page`. This
//! module writes that form ([`export_annotation`]) and applies the server's copies of other
//! users' annotations ([`apply_remote`], [`remove_by_name`]).
//!
//! The dialect is the one PDFTron WebViewer writes, which the /viewer reads from the same rows:
//! a stamp's picture is `<imagedata>` (a data URL), and extra values are the
//! `<trn-custom-data bytes='{"key":"string"}'/>` string map. PdfCraft's own values ride the same
//! channel so a round trip loses nothing:
//!
//! | PDF                                   | custom data key                              |
//! |---------------------------------------|----------------------------------------------|
//! | `/PpmStamp` (host stamp meta, JSON)   | `ppm-stamp`, or `ppm-signature` for a signature (JSON with `date` and no `templateId`) |
//! | `/PpmSignRole` (sign box, FreeText)   | `ppmSignRole`                                |
//! | `/IRT` of a non-note (caret on a strike-out) | `ppmIrt` (parent `/NM`); the server allows `inreplyto` on notes only |
//! | fork-only keys (`PCOutline`, `PCStamp`…) | `ppmFork` (JSON object)                   |
//! | any other key (kept verbatim)         | stored in `/PpmCustomData` (JSON object)     |
//!
//! Remote application never goes through ownership: the item's author becomes `/PpmOwner`, the
//! `/NM` is kept, and a locked or foreign annotation is replaced all the same (the server is
//! the authority).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use base64::Engine as _;
use pdfcraft_annot::ownership::OWNER_KEY;
use serde_json::{Map, Number, Value};

use super::*;

/// The most characters (UTF-16 code units, as the server counts them) one item's XFDF may have.
pub const MAX_XFDF_LEN: usize = 200_000;
/// Longest annotation id the server accepts.
const MAX_ID_LEN: usize = 200;
/// Longest picture a remote item may carry, decoded.
const MAX_PICTURE_BYTES: usize = 8 * 1024 * 1024;

const PREFIX: &str = r#"<?xml version="1.0" encoding="UTF-8" ?><xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve"><fields /><annots>"#;
const SUFFIX: &str = "</annots></xfdf>";

const STAMP_KEY: &str = "PpmStamp";
const SIGN_ROLE_KEY: &str = "PpmSignRole";
const CUSTOM_KEY: &str = "PpmCustomData";
/// Hash of the data URL a picture stamp was made from, so a re-applied item reuses its picture.
const PICTURE_HASH_KEY: &str = "PpmImageHash";

/// One annotation as the server stores it.
#[derive(Clone, Debug, PartialEq)]
pub struct AnnotXfdf {
    /// The annotation's `/NM`.
    pub annot_id: String,
    /// Zero-based page.
    pub page: usize,
    /// What /viewer calls the type: the `/Subj`, else the XFDF element name.
    pub kind: String,
    /// The whole XFDF document.
    pub xfdf: String,
}

/// An annotation of another user, as the server sends it.
#[derive(Clone, Copy, Debug)]
pub struct RemoteItem<'a> {
    pub annot_id: &'a str,
    pub author_id: &'a str,
    pub xfdf: &'a str,
}

/// What [`apply_remote`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteReport {
    /// Ids placed (inserted or replaced).
    pub applied: Vec<String>,
    /// Ids of the requested deletes that were present.
    pub removed: Vec<String>,
    /// (id, reason) for items that could not be applied. The document keeps what it had.
    pub failed: Vec<(String, String)>,
}

pub(crate) fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

// ── locating ────────────────────────────────────────────────────────────────────────────────

/// Where an annotation sits: page index, page object, index in the page's `/Annots`, object.
struct Spot {
    page: usize,
    page_ref: ObjRef,
    index: usize,
    obj: ObjRef,
}

/// Every annotation named `nm`, in page order.
fn locate(doc: &Document, nm: &str) -> Vec<Spot> {
    let mut found = Vec::new();
    for (page, p) in pdfcraft_model::pages(doc).iter().enumerate() {
        let list = p.dict.get(b"Annots").map(|a| doc.resolve(a)).and_then(|a| a.as_array().cloned()).unwrap_or_default();
        for (index, a) in list.iter().enumerate() {
            if let Some(obj) = a.as_ref()
                && doc.get(obj).as_dict().and_then(|d| text_of(doc, d, b"NM")).as_deref() == Some(nm)
            {
                found.push(Spot { page, page_ref: p.obj, index, obj });
            }
        }
    }
    found
}

// ── export ──────────────────────────────────────────────────────────────────────────────────

/// The annotation `nm` as a standalone XFDF document the server accepts.
///
/// Errors: [`DataError::NotFound`], [`DataError::TooLarge`] (over [`MAX_XFDF_LEN`], typically a
/// big stamp picture), [`DataError::Invalid`] (an id longer than the server allows, a reply
/// whose parent has no `/NM`, or a kind with no XFDF form such as a link).
pub fn export_annotation(doc: &Document, nm: &str) -> Result<AnnotXfdf, DataError> {
    if utf16_len(nm) > MAX_ID_LEN {
        return Err(DataError::Invalid("the annotation name is too long".into()));
    }
    let pages = pdfcraft_model::pages(doc);
    let names = annot_names(doc, &pages);
    let Some(spot) = locate(doc, nm).into_iter().next() else { return Err(DataError::NotFound(nm.into())) };
    let d = doc.get(spot.obj).as_dict().cloned().unwrap_or_default();
    if let Some(parent) = d.get(b"IRT").and_then(Object::as_ref)
        && !names.contains_key(&parent)
    {
        return Err(DataError::Invalid("the reply's parent has no name".into()));
    }
    let mut element = String::new();
    let Some(tag) = annot_element(doc, &d, spot.page, &names, true, &mut element) else {
        return Err(DataError::Invalid("this kind of annotation has no XFDF form".into()));
    };
    let xfdf = format!("{PREFIX}{}{SUFFIX}", element.trim_end());
    let len = utf16_len(&xfdf);
    if len > MAX_XFDF_LEN {
        return Err(DataError::TooLarge { len, max: MAX_XFDF_LEN });
    }
    let kind = text_of(doc, &d, b"Subj").filter(|s| !s.trim().is_empty()).unwrap_or_else(|| tag.to_string());
    Ok(AnnotXfdf { annot_id: nm.into(), page: spot.page, kind: kind.chars().take(100).collect(), xfdf })
}

/// The children after the generic ones: the stamp picture and the custom data.
pub(crate) fn rich_children(doc: &Document, d: &Dict, tag: &str, irt_name: Option<&str>, out: &mut String) {
    if tag == "stamp"
        && let Some(url) = picture_url(doc, d)
    {
        let _ = write!(out, "<imagedata>{url}</imagedata>");
    }
    let map = custom_map(doc, d, tag, irt_name);
    if !map.is_empty()
        && let Ok(json) = serde_json::to_string(&Value::Object(map))
    {
        let _ = write!(out, "<trn-custom-data bytes=\"{}\"/>", esc(&json));
    }
}

/// A picture stamp's image as a data URL.
fn picture_url(doc: &Document, d: &Dict) -> Option<String> {
    if !matches!(d.get(b"PCPictureImage").map(|o| doc.resolve(o)).as_deref(), Some(Object::Bool(true))) {
        return None;
    }
    let (ext, bytes) = pdfcraft_create::image_file(doc, d.reference(b"PCPicture")?).ok()?;
    let mime = if ext == "jpg" { "image/jpeg" } else { "image/png" };
    Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// How a fork-only key is carried in the `ppmFork` JSON object.
#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Int,
    Text,
    Name,
    Nums,
    NumLists,
    /// A border-effect dictionary (`/BE`): the cloud style of a cloud polygon.
    Effect,
}

/// Keys PdfCraft's own annotations carry that XFDF has no field for.
const FORK_KEYS: [(&str, Kind); 10] = [
    ("IT", Kind::Name),
    ("PCStamp", Kind::Bool),
    ("PCByLine", Kind::Text),
    ("PCStampRotate", Kind::Int),
    ("PCFillSign", Kind::Bool),
    ("PCOutline", Kind::NumLists),
    ("PCPictureImage", Kind::Bool),
    ("PCPictureSize", Kind::Nums),
    ("PCPictureRotate", Kind::Int),
    ("BE", Kind::Effect),
];

fn num(v: f64) -> Value {
    Number::from_f64(v).map_or(Value::Null, Value::Number)
}

fn fork_value(doc: &Document, d: &Dict, key: &str, kind: Kind) -> Option<Value> {
    let k = key.as_bytes();
    Some(match kind {
        Kind::Bool => match d.get(k).map(|o| doc.resolve(o))?.as_ref() {
            Object::Bool(b) => Value::Bool(*b),
            _ => return None,
        },
        Kind::Int => Value::from(d.get(k).and_then(|o| doc.resolve(o).as_int())?),
        Kind::Text => Value::String(text_of(doc, d, k)?),
        Kind::Name => Value::String(String::from_utf8_lossy(d.name(k)?).into_owned()),
        Kind::Nums => Value::Array(nums_of(doc, d.get(k)).into_iter().map(num).collect()),
        Kind::Effect => {
            let be = d.get(k).map(|o| doc.resolve(o))?.as_dict().cloned()?;
            let mut m = Map::new();
            m.insert("S".into(), Value::String(String::from_utf8_lossy(be.name(b"S")?).into_owned()));
            m.insert("I".into(), be.get(b"I").and_then(|i| doc.resolve(i).as_f64()).map_or(Value::Null, num));
            Value::Object(m)
        }
        Kind::NumLists => {
            let list = d.get(k).map(|o| doc.resolve(o)).and_then(|o| o.as_array().cloned())?;
            Value::Array(list.iter().map(|g| Value::Array(nums_of(doc, Some(g)).into_iter().map(num).collect())).collect())
        }
    })
}

fn fork_object(v: &Value, kind: Kind) -> Option<Object> {
    let nums = |a: &Value| -> Option<Vec<f64>> { a.as_array()?.iter().map(Value::as_f64).collect() };
    Some(match kind {
        Kind::Bool => Object::Bool(v.as_bool()?),
        Kind::Int => Object::Int(v.as_i64()?),
        Kind::Text => PdfString::text(v.as_str()?).into(),
        Kind::Name => Object::name(v.as_str()?),
        Kind::Nums => arr(&nums(v)?),
        Kind::Effect => {
            let mut be = Dict::new();
            be.set(b"S".to_vec(), Object::name(v.get("S")?.as_str()?));
            if let Some(i) = v.get("I").and_then(Value::as_f64) {
                be.set(b"I".to_vec(), Object::Int(i as i64));
            }
            Object::Dict(be)
        }
        Kind::NumLists => Object::Array(v.as_array()?.iter().map(|g| nums(g).map(|n| arr(&n))).collect::<Option<Vec<_>>>()?),
    })
}

/// The custom data map of an annotation: what it held when it came in, plus its own values.
fn custom_map(doc: &Document, d: &Dict, tag: &str, irt_name: Option<&str>) -> Map<String, Value> {
    let mut map: Map<String, Value> = text_of(doc, d, CUSTOM_KEY.as_bytes())
        .and_then(|j| serde_json::from_str::<Value>(&j).ok())
        .and_then(|v| if let Value::Object(m) = v { Some(m) } else { None })
        .unwrap_or_default();
    if let Some(meta) = text_of(doc, d, STAMP_KEY.as_bytes()) {
        // A host signature carries only its date; a stamp carries the template it was made from.
        let signature = serde_json::from_str::<Value>(&meta).is_ok_and(|v| v.get("templateId").is_none() && v.get("date").is_some());
        map.insert((if signature { "ppm-signature" } else { "ppm-stamp" }).into(), Value::String(meta));
    }
    if tag == "freetext"
        && let Some(role) = text_of(doc, d, SIGN_ROLE_KEY.as_bytes())
    {
        map.insert("ppmSignRole".into(), Value::String(role));
    }
    if let Some(parent) = irt_name {
        map.insert("ppmIrt".into(), Value::String(parent.into()));
    }
    let fork: Map<String, Value> = FORK_KEYS.iter().filter_map(|(k, kind)| fork_value(doc, d, k, *kind).map(|v| ((*k).to_string(), v))).collect();
    // The kind-specific keys only matter where the generic XFDF fields can't rebuild them.
    if !fork.is_empty()
        && let Ok(json) = serde_json::to_string(&Value::Object(fork))
    {
        map.insert("ppmFork".into(), Value::String(json));
    }
    map
}

// ── remote application ──────────────────────────────────────────────────────────────────────

/// Remove the annotation `nm` with its pop-up and replies (and theirs), whatever its lock flag
/// or owner. `true` when something was removed.
pub fn remove_by_name(doc: &mut Document, nm: &str) -> bool {
    let mut removed = false;
    // A name can repeat (a file from elsewhere); each pass removes one, so bound the passes.
    for _ in 0..1000 {
        let Some(spot) = locate(doc, nm).into_iter().next() else { break };
        if pdfcraft_annot::delete_annotation_unchecked(doc, spot.page, spot.index).is_err() {
            break;
        }
        removed = true;
    }
    removed
}

/// Apply the server's annotations: first `deletes` (by `/NM`), then `upserts` (inserted, or
/// replacing the annotation with the same `/NM`, even on another page).
///
/// Each upsert's `/PpmOwner` becomes its `author_id`, the `/NM` is kept, the appearance is
/// redrawn, a pop-up and the reply link (`inreplyto`, or `ppmIrt` for a caret) are rebuilt.
/// Replies may come before their parent in the slice. Nothing here checks ownership or locks,
/// and applying the same batch twice, or onto a snapshot that lacks the annotation, is fine.
pub fn apply_remote(doc: &mut Document, upserts: &[RemoteItem], deletes: &[String]) -> RemoteReport {
    let mut report = RemoteReport::default();
    for nm in deletes {
        if remove_by_name(doc, nm) {
            report.removed.push(nm.clone());
        }
    }
    let mut by_name: HashMap<&str, ObjRef> = HashMap::new();
    // (reply, parent name, id) to link once every parent of the batch is placed.
    let mut replies: Vec<(ObjRef, String, &str)> = Vec::new();
    for item in upserts {
        match place_item(doc, item) {
            Ok(placed) => {
                by_name.insert(item.annot_id, placed.obj);
                if let Some(parent) = placed.parent {
                    replies.push((placed.obj, parent, item.annot_id));
                }
                report.applied.push(item.annot_id.into());
            }
            Err(why) => report.failed.push((item.annot_id.into(), why)),
        }
    }
    for (reply, parent, id) in replies {
        let target = by_name.get(parent.as_str()).copied().or_else(|| find_by_name(doc, &parent));
        let linked = target.filter(|t| *t != reply).is_some_and(|t| doc.update_dict(reply, |d| d.set(b"IRT".to_vec(), Object::Ref(t))).is_ok());
        if !linked {
            // A reply with no parent would show as a top-level note; leave it out.
            remove_by_name(doc, id);
            report.applied.retain(|a| a != id);
            report.failed.push((id.into(), format!("its parent {parent} is missing")));
        }
    }
    report
}

struct Placed {
    obj: ObjRef,
    /// The `/NM` this annotation replies to.
    parent: Option<String>,
}

fn place_item(doc: &mut Document, item: &RemoteItem) -> Result<Placed, String> {
    check_form_xml_depth(item.xfdf).map_err(|e| e.to_string())?;
    let xml = parse_form_xml(item.xfdf).map_err(|e| e.to_string())?;
    let root = xml.root_element();
    if root.tag_name().name() != "xfdf" {
        return Err("not an XFDF document".into());
    }
    let annots = root.children().find(|c| c.has_tag_name("annots")).ok_or("no annots element")?;
    let mut elements = annots.children().filter(|c| c.is_element());
    let (Some(node), None) = (elements.next(), elements.next()) else { return Err("annots must hold exactly one annotation".into()) };
    if node.attribute("name") != Some(item.annot_id) || item.annot_id.is_empty() {
        return Err("the annotation's name differs from its id".into());
    }
    let tag = node.tag_name().name().to_ascii_lowercase();
    let (subtype, _) = SUBTYPES.iter().find(|(_, t)| *t == tag).ok_or_else(|| format!("unknown annotation kind {tag}"))?;
    let pages: Vec<ObjRef> = pdfcraft_model::pages(doc).iter().map(|p| p.obj).collect();
    let page_ref =
        node.attribute("page").and_then(|p| p.parse::<usize>().ok()).and_then(|p| pages.get(p).copied()).ok_or("the page is outside the document")?;
    let mut d = annot_from_xml(node, subtype, page_ref).ok_or("the annotation has no usable rectangle")?;
    d.set(OWNER_KEY.as_bytes().to_vec(), PdfString::text(item.author_id));
    // As in a file import: a redaction mark someone else made is always shown to the user.
    if d.name(b"Subtype") == Some(b"Redact".as_slice()) {
        let flags = d.int(b"F").unwrap_or(0);
        d.set(b"F".to_vec(), Object::Int(flags & !REDACT_UNSEEN));
    }

    let mut parent = node.attribute("inreplyto").filter(|p| !p.is_empty()).map(str::to_string);
    if let Some(custom) = read_custom(node)
        && let Some(irt) = apply_custom(&mut d, &custom)
    {
        parent = parent.or(Some(irt));
    }
    let existing = locate(doc, item.annot_id);
    if let Some(url) = node.children().find(|c| c.has_tag_name("imagedata")).and_then(|c| c.text()) {
        let reuse = existing.first().map(|s| s.obj);
        attach_picture(doc, &mut d, url.trim(), reuse)?;
    }

    // Replace in place (keeping the object, so replies that point at it stay linked), moving it
    // between pages when it moved. Duplicates of the name go.
    let mut obj = None;
    for (i, spot) in existing.iter().enumerate() {
        let old = doc.get(spot.obj).as_dict().cloned().unwrap_or_default();
        let popup = old.reference(b"Popup");
        // The first copy keeps its slot when it stays on its page; every other entry goes.
        let keep_here = i == 0 && spot.page_ref == page_ref;
        let kept: Vec<Object> = annots_of(doc, spot.page_ref)
            .into_iter()
            .filter(|e| e.as_ref().is_none_or(|r| popup != Some(r) && (r != spot.obj || keep_here)))
            .collect();
        doc.update_dict(spot.page_ref, |p| p.set(b"Annots".to_vec(), Object::Array(kept))).map_err(|e| e.to_string())?;
        if i == 0 {
            obj = Some(spot.obj);
        }
    }
    let r = match obj {
        Some(r) => {
            doc.set(r, Object::Dict(d));
            r
        }
        None => doc.add(Object::Dict(d)),
    };
    let mut list = annots_of(doc, page_ref);
    if !list.iter().any(|e| e.as_ref() == Some(r)) {
        list.push(Object::Ref(r));
        doc.update_dict(page_ref, |p| p.set(b"Annots".to_vec(), Object::Array(list))).map_err(|e| e.to_string())?;
    }
    if let Some(pop) = node.children().find(|c| c.has_tag_name("popup")) {
        add_popup(doc, page_ref, r, pop).map_err(|e| e.to_string())?;
    }
    // An annotation whose appearance can't be drawn stays (it is the user's data), undrawn.
    let _ = pdfcraft_annot::set_appearance(doc, r);
    Ok(Placed { obj: r, parent })
}

fn annots_of(doc: &Document, page: ObjRef) -> Vec<Object> {
    doc.get(page).as_dict().and_then(|p| p.get(b"Annots").cloned()).map(|a| doc.resolve(&a)).and_then(|a| a.as_array().cloned()).unwrap_or_default()
}

/// The `<trn-custom-data>` string map of an annotation element.
fn read_custom(node: roxmltree::Node) -> Option<Map<String, Value>> {
    let bytes = node.children().find(|c| c.has_tag_name("trn-custom-data"))?.attribute("bytes")?;
    match serde_json::from_str::<Value>(bytes).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Put custom data into private keys (see the module table). Returns the `ppmIrt` parent.
fn apply_custom(d: &mut Dict, custom: &Map<String, Value>) -> Option<String> {
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut rest = Map::new();
    let mut parent = None;
    for (k, v) in custom {
        match k.as_str() {
            "ppm-stamp" | "ppm-signature" => d.set(STAMP_KEY.as_bytes().to_vec(), PdfString::text(&text(v))),
            "ppmSignRole" => d.set(SIGN_ROLE_KEY.as_bytes().to_vec(), PdfString::text(&text(v))),
            "ppmIrt" => parent = Some(text(v)),
            "ppmFork" => {
                if let Ok(Value::Object(fork)) = serde_json::from_str::<Value>(&text(v)) {
                    for (key, kind) in FORK_KEYS {
                        if let Some(o) = fork.get(key).and_then(|x| fork_object(x, kind)) {
                            d.set(key.as_bytes().to_vec(), o);
                        }
                    }
                }
            }
            _ => {
                rest.insert(k.clone(), v.clone());
            }
        }
    }
    if !rest.is_empty()
        && let Ok(json) = serde_json::to_string(&Value::Object(rest))
    {
        d.set(CUSTOM_KEY.as_bytes().to_vec(), PdfString::text(&json));
    }
    parent
}

fn hash_of(url: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}-{}", h.finish(), url.len())
}

/// Make `d` a picture stamp showing the data URL `url`. `reuse` is the object being replaced:
/// when it was made from the same URL its picture is kept instead of adding the image again.
fn attach_picture(doc: &mut Document, d: &mut Dict, url: &str, reuse: Option<ObjRef>) -> Result<(), String> {
    let hash = hash_of(url);
    let old = reuse.and_then(|r| doc.get(r).as_dict().cloned());
    if let Some(old) = old.filter(|o| text_of(doc, o, PICTURE_HASH_KEY.as_bytes()).as_deref() == Some(&hash))
        && let Some(pic) = old.reference(b"PCPicture")
    {
        d.set(b"PCPicture".to_vec(), Object::Ref(pic));
        for key in [&b"PCPictureSize"[..], b"PCPictureImage"] {
            if let Some(v) = old.get(key) {
                d.set(key.to_vec(), v.clone());
            }
        }
        d.set(PICTURE_HASH_KEY.as_bytes().to_vec(), PdfString::text(&hash));
        return Ok(());
    }
    let (header, data) = url.strip_prefix("data:").and_then(|u| u.split_once(',')).ok_or("the stamp picture is not a data URL")?;
    if !header.contains(";base64") {
        return Err("the stamp picture is not base64".into());
    }
    let clean: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() / 4 * 3 > MAX_PICTURE_BYTES {
        return Err("the stamp picture is too large".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(clean).map_err(|e| format!("the stamp picture: {e}"))?;
    let (xobject, size) = pdfcraft_create::image_xobject(doc, "stamp.png", &bytes).map_err(|e| e.to_string())?;
    d.set(b"PCPicture".to_vec(), Object::Ref(xobject));
    d.set(b"PCPictureImage".to_vec(), Object::Bool(true));
    d.set(b"PCPictureSize".to_vec(), arr(&[size.0, size.1]));
    if d.get(b"Name").is_none() {
        d.set(b"Name".to_vec(), Object::name("PCCustomStamp"));
    }
    d.set(PICTURE_HASH_KEY.as_bytes().to_vec(), PdfString::text(&hash));
    Ok(())
}
