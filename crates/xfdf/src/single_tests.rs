use pdfcraft_annot::ownership::OWNER_KEY;
use pdfcraft_annot::{
    LineEnding, Markup, Meta, NewAnnotation, NoteIcon, Shape, Style, add_annotation, add_reply, add_text_replacement, set_extra, set_locked,
    set_review_state, summaries,
};

use super::*;
use crate::single::utf16_len;
use crate::tests::blank;

fn meta(id: &str) -> Meta {
    Meta { date: Some("D:20261023120000+09'00'".into()), id: id.into() }
}

fn add(doc: &mut Document, page: usize, shape: Shape, contents: &str, id: &str) -> usize {
    let style = Style::default_for(&shape);
    add_annotation(doc, &NewAnnotation { page, shape, style, contents: contents.into(), author: "Ada".into() }, &meta(id)).unwrap()
}

/// The server's rules for one stored item, mirrored from `packages/contracts/src/xfdf.ts`:
/// a well-formed document with one element inside `annots`, `name` = id, a zero-based numeric
/// `page`, no DOCTYPE/comment/processing instruction (but the leading declaration) and no control
/// characters, within the size limit.
fn assert_server_rules(item: &AnnotXfdf) {
    let x = &item.xfdf;
    assert!(utf16_len(x) <= MAX_XFDF_LEN, "size");
    assert!(!x.chars().any(|c| (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r')), "control character");
    assert!(!x.contains("<!") && x.matches("<?").count() == 1 && x.starts_with("<?xml"), "doctype, comment or processing instruction");
    let doc = roxmltree::Document::parse(x).unwrap_or_else(|e| panic!("not well-formed: {e}\n{x}"));
    let root = doc.root_element();
    assert_eq!(root.tag_name().name(), "xfdf");
    let annots: Vec<_> = root.children().filter(|c| c.has_tag_name("annots")).collect();
    assert_eq!(annots.len(), 1, "exactly one annots element");
    assert!(annots[0].children().all(|c| c.is_element() || c.text().is_none_or(|t| t.trim().is_empty())), "text inside annots");
    let items: Vec<_> = annots[0].children().filter(|c| c.is_element()).collect();
    assert_eq!(items.len(), 1, "exactly one annotation element: {x}");
    assert_eq!(items[0].attribute("name"), Some(item.annot_id.as_str()), "name equals the id");
    let page = items[0].attribute("page").unwrap_or("");
    assert!(page.chars().all(|c| c.is_ascii_digit()) && !page.is_empty(), "page attribute");
    assert_eq!(page.parse::<usize>().unwrap(), item.page, "zero-based page");
    assert!(!item.annot_id.is_empty() && utf16_len(&item.annot_id) <= 200);
    assert!(!item.kind.is_empty() && item.kind.chars().count() <= 100);
}

/// Export `nm` from `src`, check the server rules, apply it to `dst` as `author`, export it again
/// and require the same document back.
fn round_trip(src: &Document, dst: &mut Document, nm: &str) -> AnnotXfdf {
    let first = export_annotation(src, nm).unwrap();
    assert_server_rules(&first);
    let report = apply_remote(dst, &[RemoteItem { annot_id: nm, author_id: "u-a", xfdf: &first.xfdf }], &[]);
    assert_eq!(report.failed, vec![], "{}", first.xfdf);
    let second = export_annotation(dst, nm).unwrap();
    assert_eq!(first, second, "round trip changed the item");
    first
}

fn names(doc: &Document) -> Vec<String> {
    summaries(doc).into_iter().filter_map(|s| s.name).collect()
}

fn owner_of_name(doc: &Document, nm: &str) -> Option<String> {
    let r = find_by_name(doc, nm)?;
    pdfcraft_annot::ownership::owner_of(doc, doc.get(r).as_dict()?)
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().unwrap();
    let data: Vec<u8> = (0..w * h).flat_map(|i| [(i * 7 % 256) as u8, 40, 200, if i % 3 == 0 { 255 } else { 128 }]).collect();
    writer.write_image_data(&data).unwrap();
    drop(writer);
    out
}

fn stamp(doc: &mut Document, page: usize, id: &str, meta_json: Option<&str>) {
    let (xobject, size) = pdfcraft_create::image_xobject(doc, "stamp.png", &png(8, 6)).unwrap();
    let shape = Shape::CustomStamp { rect: [50.0, 50.0, 150.0, 100.0], name: "Approved".into(), picture: xobject, image: true, size };
    let index = add(doc, page, shape, "Approved", id);
    if let Some(m) = meta_json {
        set_extra(doc, page, index, "PpmStamp", m).unwrap();
    }
}

#[test]
fn every_supported_subtype_round_trips_and_passes_the_server_rules() {
    let mut src = blank();
    let quad = [10.0, 110.0, 90.0, 110.0, 10.0, 100.0, 90.0, 100.0];
    let shapes: Vec<(&str, Shape)> = vec![
        ("note", Shape::Note { at: [100.0, 500.0], icon: NoteIcon::Comment }),
        ("hl", Shape::TextMarkup { kind: Markup::Highlight, quads: vec![quad] }),
        ("ul", Shape::TextMarkup { kind: Markup::Underline, quads: vec![quad] }),
        ("so", Shape::TextMarkup { kind: Markup::StrikeOut, quads: vec![quad] }),
        ("sq", Shape::TextMarkup { kind: Markup::Squiggly, quads: vec![quad] }),
        ("rect", Shape::Rectangle { rect: [10.0, 10.0, 80.0, 60.0] }),
        ("oval", Shape::Oval { rect: [10.0, 10.0, 80.0, 60.0] }),
        ("line", Shape::Line { from: [10.0, 10.0], to: [90.0, 70.0], start: LineEnding::None, end: LineEnding::OpenArrow }),
        ("ink", Shape::Ink { strokes: vec![vec![[10.0, 10.0], [20.0, 30.0], [40.0, 20.0]]] }),
        ("tb", Shape::TextBox { rect: [20.0, 200.0, 200.0, 260.0], font_size: 12.0 }),
        ("poly", Shape::Polygon { vertices: vec![[10.0, 10.0], [60.0, 10.0], [35.0, 50.0]], cloud: false }),
        ("pline", Shape::PolyLine { vertices: vec![[10.0, 10.0], [60.0, 40.0], [90.0, 10.0]], start: LineEnding::None, end: LineEnding::None }),
        (
            "callout",
            Shape::Callout {
                rect: [100.0, 300.0, 220.0, 340.0],
                knee: [90.0, 320.0],
                point: [60.0, 300.0],
                font_size: 12.0,
                ending: LineEnding::OpenArrow,
            },
        ),
        ("caret", Shape::Caret { rect: [30.0, 30.0, 40.0, 40.0] }),
    ];
    let mut dst = blank();
    for (i, (id, shape)) in shapes.into_iter().enumerate() {
        add(&mut src, i % 2, shape, "text & <more>", id);
        let item = round_trip(&src, &mut dst, id);
        assert_eq!(item.page, i % 2);
        assert_eq!(owner_of_name(&dst, id).as_deref(), Some("u-a"), "{id}");
        // Drawn on import: every one of these has an appearance of ours.
        let r = find_by_name(&dst, id).unwrap();
        assert!(dst.get(r).as_dict().unwrap().get(b"AP").is_some(), "{id} has no appearance");
    }
}

#[test]
fn stamp_picture_travels_as_imagedata_and_is_drawn_on_import() {
    let mut src = blank();
    stamp(&mut src, 1, "stamp1", Some(r#"{"templateId":"3f2b1c1e-8f7d-4c53-a0a5-0a0a0a0a0a0a","version":2,"values":{},"sig":"abc"}"#));
    let item = round_trip(&src, &mut blank(), "stamp1");
    assert_eq!(item.page, 1);
    assert!(item.xfdf.contains("<imagedata>data:image/png;base64,"), "{}", item.xfdf);
    assert!(item.xfdf.contains("<stamp "));

    let mut dst = blank();
    apply_remote(&mut dst, &[RemoteItem { annot_id: "stamp1", author_id: "u-b", xfdf: &item.xfdf }], &[]);
    let r = find_by_name(&dst, "stamp1").unwrap();
    let d = dst.get(r).as_dict().unwrap().clone();
    let pic = d.reference(b"PCPicture").expect("picture");
    let Object::Stream(s) = &*dst.get(pic) else { panic!("not an image") };
    assert_eq!((s.dict.int(b"Width"), s.dict.int(b"Height")), (Some(8), Some(6)));
    assert!(d.get(b"AP").is_some());
    // The image is not added again when the same item is applied twice.
    let before = dst.get(r).as_dict().unwrap().reference(b"PCPicture");
    apply_remote(&mut dst, &[RemoteItem { annot_id: "stamp1", author_id: "u-b", xfdf: &item.xfdf }], &[]);
    assert_eq!(dst.get(r).as_dict().unwrap().reference(b"PCPicture"), before);
}

#[test]
fn a_plain_viewer_stamp_with_only_imagedata_is_drawn() {
    // What /viewer (PDFTron) stores: no fork keys at all.
    let url = format!("data:image/png;base64,{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png(4, 4)));
    let x = format!(
        r#"<?xml version="1.0" encoding="UTF-8" ?><xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve"><fields /><annots><stamp page="0" name="v1" rect="10,10,110,60" title="Kim" subject="Stamp" date="D:20260101120000+09'00'" flags="print"><trn-custom-data bytes="{{&quot;ppm-signature&quot;:&quot;{{\&quot;date\&quot;:\&quot;2026-01-01\&quot;}}&quot;}}"/><imagedata>{url}</imagedata></stamp></annots></xfdf>"#
    );
    let mut dst = blank();
    let rep = apply_remote(&mut dst, &[RemoteItem { annot_id: "v1", author_id: "u-k", xfdf: &x }], &[]);
    assert_eq!(rep.applied, vec!["v1".to_string()], "{rep:?}");
    let r = find_by_name(&dst, "v1").unwrap();
    assert!(dst.get(r).as_dict().unwrap().get(b"AP").is_some());
    let back = export_annotation(&dst, "v1").unwrap();
    assert_server_rules(&back);
    assert!(back.xfdf.contains("ppm-signature"), "a signature keeps its key: {}", back.xfdf);
}

#[test]
fn custom_data_round_trips() {
    let mut src = blank();
    stamp(&mut src, 0, "sg", Some(r#"{"date":"2026-10-23"}"#));
    let item = round_trip(&src, &mut blank(), "sg");
    assert!(item.xfdf.contains("ppm-signature") && !item.xfdf.contains("ppm-stamp"), "{}", item.xfdf);

    // A sign box: a FreeText with a role, plus a key the fork does not know (kept verbatim).
    add(&mut src, 0, Shape::TextBox { rect: [20.0, 200.0, 200.0, 260.0], font_size: 12.0 }, "Sign here", "box");
    let idx = summaries(&src).iter().find(|s| s.name.as_deref() == Some("box")).unwrap().index;
    set_extra(&mut src, 0, idx, "PpmSignRole", "approver").unwrap();
    set_extra(&mut src, 0, idx, "PpmCustomData", r#"{"trn-unrotated-rect":"1,2,3,4"}"#).unwrap();
    let item = round_trip(&src, &mut blank(), "box");
    assert!(item.xfdf.contains("ppmSignRole") && item.xfdf.contains("trn-unrotated-rect"), "{}", item.xfdf);
}

#[test]
fn fork_only_stamps_keep_their_look_through_the_database_form() {
    let mut src = blank();
    let contour = vec![[0.1, 0.1], [0.9, 0.1], [0.5, 0.9]];
    add(&mut src, 0, Shape::TypedSignature { rect: [10.0, 10.0, 110.0, 50.0], contours: vec![contour] }, "", "typed");
    let mut dst = blank();
    round_trip(&src, &mut dst, "typed");
    let r = find_by_name(&dst, "typed").unwrap();
    let d = dst.get(r).as_dict().unwrap().clone();
    assert!(d.get(b"PCOutline").is_some() && d.get(b"AP").is_some());
}

#[test]
fn replies_link_by_name_whatever_the_order() {
    let mut src = blank();
    let note = add(&mut src, 0, Shape::Note { at: [100.0, 500.0], icon: NoteIcon::Comment }, "question", "parent");
    add_reply(&mut src, 0, note, "answer", "Bob", &meta("reply")).unwrap();
    set_review_state(&mut src, 0, note, pdfcraft_annot::ReviewState::Accepted, "Bob", &meta("status")).unwrap();
    let reply = export_annotation(&src, "reply").unwrap();
    assert_server_rules(&reply);
    assert!(reply.xfdf.contains(r#"inreplyto="parent""#), "{}", reply.xfdf);
    let status = export_annotation(&src, "status").unwrap();
    assert!(
        status.xfdf.contains(r#"inreplyto="parent""#) && status.xfdf.contains("state=\"Accepted\"") && status.xfdf.contains("statemodel=\"Review\"")
    );
    let parent = export_annotation(&src, "parent").unwrap();

    // The reply and the status come before the parent.
    let mut dst = blank();
    let items = [
        RemoteItem { annot_id: "reply", author_id: "u-b", xfdf: &reply.xfdf },
        RemoteItem { annot_id: "status", author_id: "u-b", xfdf: &status.xfdf },
        RemoteItem { annot_id: "parent", author_id: "u-a", xfdf: &parent.xfdf },
    ];
    let rep = apply_remote(&mut dst, &items, &[]);
    assert_eq!(rep.failed, vec![]);
    let s = summaries(&dst);
    let reply_row = s.iter().find(|r| r.name.as_deref() == Some("reply")).unwrap();
    assert_eq!(reply_row.in_reply_to.as_deref(), Some("parent"));
    assert_eq!(owner_of_name(&dst, "reply").as_deref(), Some("u-b"));
    assert_eq!(owner_of_name(&dst, "parent").as_deref(), Some("u-a"));

    // Deleting the parent takes its replies, locked or not.
    let mut again = dst.clone();
    assert!(remove_by_name(&mut again, "parent"));
    assert!(names(&again).is_empty(), "{:?}", names(&again));
}

#[test]
fn a_reply_without_its_parent_is_refused_not_shown_as_top_level() {
    let mut src = blank();
    let note = add(&mut src, 0, Shape::Note { at: [100.0, 500.0], icon: NoteIcon::Comment }, "q", "p");
    add_reply(&mut src, 0, note, "a", "Bob", &meta("r")).unwrap();
    let reply = export_annotation(&src, "r").unwrap();
    let mut dst = blank();
    let rep = apply_remote(&mut dst, &[RemoteItem { annot_id: "r", author_id: "u-b", xfdf: &reply.xfdf }], &[]);
    assert_eq!(rep.failed.len(), 1);
    assert!(names(&dst).is_empty());

    // A reply whose parent has no /NM cannot be exported as a reply.
    let (_, parent) = (0, find_by_name(&src, "p").unwrap());
    src.update_dict(parent, |d| {
        d.remove(b"NM");
    })
    .unwrap();
    assert!(matches!(export_annotation(&src, "r"), Err(DataError::Invalid(_))));
}

#[test]
fn text_replacement_is_two_top_level_items_that_relink() {
    let mut src = blank();
    let style = Style { color: [0.89, 0.13, 0.13], opacity: 1.0, width: 1.0, fill: None };
    let quads = [[100.0, 720.0, 200.0, 720.0, 100.0, 706.0, 200.0, 706.0]];
    add_text_replacement(&mut src, 0, &quads, "new words", "Ada", &style, &style, &meta("strike"), &meta("caret")).unwrap();
    let caret = export_annotation(&src, "caret").unwrap();
    assert_server_rules(&caret);
    assert!(!caret.xfdf.contains("inreplyto"), "the server allows replies on notes only: {}", caret.xfdf);
    assert!(caret.xfdf.contains("ppmIrt"));
    let strike = export_annotation(&src, "strike").unwrap();

    let mut dst = blank();
    let items = [
        RemoteItem { annot_id: "caret", author_id: "u-a", xfdf: &caret.xfdf },
        RemoteItem { annot_id: "strike", author_id: "u-a", xfdf: &strike.xfdf },
    ];
    assert_eq!(apply_remote(&mut dst, &items, &[]).failed, vec![]);
    let s = summaries(&dst);
    assert_eq!(s.iter().find(|r| r.name.as_deref() == Some("caret")).unwrap().in_reply_to.as_deref(), Some("strike"));
    assert_eq!(export_annotation(&dst, "caret").unwrap(), caret);
    // The strikeout's delete takes the caret with it, as locally.
    assert!(remove_by_name(&mut dst, "strike"));
    assert!(names(&dst).is_empty());
}

#[test]
fn remote_apply_replaces_in_place_ignoring_locks_and_keeps_the_name() {
    let mut src = blank();
    add(&mut src, 0, Shape::Rectangle { rect: [10.0, 10.0, 80.0, 60.0] }, "v1", "box");
    let v1 = export_annotation(&src, "box").unwrap();
    let mut dst = blank();
    apply_remote(&mut dst, &[RemoteItem { annot_id: "box", author_id: "u-a", xfdf: &v1.xfdf }], &[]);
    let obj = find_by_name(&dst, "box").unwrap();
    let idx = summaries(&dst).iter().find(|s| s.name.as_deref() == Some("box")).unwrap().index;
    set_locked(&mut dst, 0, idx, true).unwrap();

    // The author edits it (and moves it to page 2): the lock and our own ownership do not stop the update.
    let mut moved = blank();
    add(&mut moved, 1, Shape::Rectangle { rect: [20.0, 20.0, 90.0, 70.0] }, "v2", "box");
    let v2 = export_annotation(&moved, "box").unwrap();
    let rep = apply_remote(&mut dst, &[RemoteItem { annot_id: "box", author_id: "u-a", xfdf: &v2.xfdf }], &[]);
    assert_eq!(rep.applied, vec!["box".to_string()]);
    assert_eq!(find_by_name(&dst, "box"), Some(obj), "the object is kept");
    assert_eq!(names(&dst), vec!["box".to_string()], "no copy left on the old page");
    let now = export_annotation(&dst, "box").unwrap();
    assert_eq!((now.page, now.xfdf.contains("v2")), (1, true));
    // Idempotent.
    apply_remote(&mut dst, &[RemoteItem { annot_id: "box", author_id: "u-a", xfdf: &v2.xfdf }], &[]);
    assert_eq!(names(&dst), vec!["box".to_string()]);
    // A locked annotation can still be deleted remotely.
    let idx = summaries(&dst).iter().find(|s| s.name.as_deref() == Some("box")).unwrap().index;
    set_locked(&mut dst, 1, idx, true).unwrap();
    let rep = apply_remote(&mut dst, &[], &["box".to_string(), "never-existed".to_string()]);
    assert_eq!(rep.removed, vec!["box".to_string()]);
    assert!(names(&dst).is_empty());
}

#[test]
fn remote_owner_replaces_a_local_one_and_never_claims() {
    let mut src = blank();
    add(&mut src, 0, Shape::Oval { rect: [10.0, 10.0, 80.0, 60.0] }, "o", "same");
    let x = export_annotation(&src, "same").unwrap();
    let mut dst = blank();
    apply_remote(&mut dst, &[RemoteItem { annot_id: "same", author_id: "u-a", xfdf: &x.xfdf }], &[]);
    apply_remote(&mut dst, &[RemoteItem { annot_id: "same", author_id: "u-b", xfdf: &x.xfdf }], &[]);
    assert_eq!(owner_of_name(&dst, "same").as_deref(), Some("u-b"));
    let r = find_by_name(&dst, "same").unwrap();
    assert!(dst.get(r).as_dict().unwrap().get(OWNER_KEY.as_bytes()).is_some());
    assert_eq!(names(&dst), vec!["same".to_string()], "the name is not regenerated");
}

#[test]
fn korean_text_boxes_are_drawn_on_import() {
    let mut src = blank();
    add(&mut src, 0, Shape::TextBox { rect: [20.0, 200.0, 220.0, 260.0], font_size: 12.0 }, "검토 부탁드립니다", "ko");
    let mut dst = blank();
    let item = round_trip(&src, &mut dst, "ko");
    assert!(item.xfdf.contains("검토 부탁드립니다"));
    let r = find_by_name(&dst, "ko").unwrap();
    assert!(dst.get(r).as_dict().unwrap().get(b"AP").is_some(), "the box has no appearance");
}

#[test]
fn bad_items_fail_without_touching_the_document() {
    let mut src = blank();
    add(&mut src, 0, Shape::Rectangle { rect: [10.0, 10.0, 80.0, 60.0] }, "r", "good");
    let good = export_annotation(&src, "good").unwrap();
    let two = good.xfdf.replace("</annots>", &format!("{}</annots>", good.xfdf.split("<annots>").nth(1).unwrap().split("</annots>").next().unwrap()));
    let doctype = good.xfdf.replacen("<xfdf", "<!DOCTYPE x [<!ENTITY a \"b\">]><xfdf", 1);
    let cases = [
        ("wrong-id", good.xfdf.clone()),
        ("good", two),
        ("good", doctype),
        ("good", good.xfdf.replace("page=\"0\"", "page=\"9\"")),
        ("good", "not xml".to_string()),
        ("good", good.xfdf.replace("<annots>", "<annots><bogus name=\"good\"/>")),
    ];
    let mut dst = blank();
    for (id, x) in &cases {
        let rep = apply_remote(&mut dst, &[RemoteItem { annot_id: id, author_id: "u", xfdf: x }], &[]);
        assert_eq!((rep.applied.len(), rep.failed.len()), (0, 1), "{id}: {x}");
    }
    assert!(names(&dst).is_empty());
}

#[test]
fn export_errors_and_size_limit() {
    let mut src = blank();
    add(&mut src, 0, Shape::Note { at: [100.0, 500.0], icon: NoteIcon::Comment }, &"가".repeat(MAX_XFDF_LEN), "big");
    assert!(matches!(export_annotation(&src, "big"), Err(DataError::TooLarge { .. })));
    assert!(matches!(export_annotation(&src, "nope"), Err(DataError::NotFound(_))));
    assert!(matches!(export_annotation(&src, &"x".repeat(201)), Err(DataError::Invalid(_))));
}

#[test]
fn full_export_is_unchanged_by_the_database_form() {
    // The user-facing XFDF export has no custom data or pictures.
    let mut src = blank();
    stamp(&mut src, 0, "s", Some(r#"{"date":"d"}"#));
    let x = export_xfdf(&src, true, false, "a.pdf");
    assert!(!x.contains("imagedata") && !x.contains("trn-custom-data"), "{x}");
}

/// The private keys that decide how a PdfCraft annotation is drawn, compared on the destination
/// dictionary itself: an export that leaves a key out would still compare equal to itself.
fn look_keys(doc: &Document, nm: &str) -> Vec<(String, String)> {
    let d = doc.get(find_by_name(doc, nm).unwrap()).as_dict().unwrap().clone();
    let mut out = Vec::new();
    for key in ["Name", "IT", "PCStamp", "PCByLine", "PCFillSign", "PCOutline", "PCPictureImage", "BE", "InkList", "Subj"] {
        if let Some(v) = d.get(key.as_bytes()) {
            out.push((key.to_string(), format!("{:?}", doc.resolve(v))));
        }
    }
    out
}

#[test]
fn fork_only_looks_survive_in_the_destination_dictionary() {
    let mut src = blank();
    let shapes: Vec<(&str, Shape)> = vec![
        ("approved", Shape::Stamp { rect: [10.0, 10.0, 150.0, 60.0], stamp: pdfcraft_annot::StampKind::Approved, by: Some("Ada".into()) }),
        ("check", Shape::Mark { rect: [10.0, 10.0, 30.0, 30.0], mark: pdfcraft_annot::FillMark::Check }),
        ("typed", Shape::Typewriter { rect: [10.0, 100.0, 200.0, 120.0], font_size: 12.0 }),
        ("drawn", Shape::Signature { strokes: vec![vec![[10.0, 10.0], [20.0, 30.0], [40.0, 20.0]]] }),
        ("cloud", Shape::Polygon { vertices: vec![[10.0, 10.0], [60.0, 10.0], [35.0, 50.0]], cloud: true }),
    ];
    let mut dst = blank();
    for (id, shape) in shapes {
        add(&mut src, 0, shape, "x", id);
        round_trip(&src, &mut dst, id);
        let (a, b) = (look_keys(&src, id), look_keys(&dst, id));
        assert!(!a.is_empty());
        assert_eq!(a, b, "{id}");
        let r = find_by_name(&dst, id).unwrap();
        assert!(dst.get(r).as_dict().unwrap().get(b"AP").is_some(), "{id} has no appearance");
    }
}

#[test]
fn a_remote_redaction_mark_is_never_hidden() {
    let mut src = blank();
    let shape = Shape::Redact {
        quads: vec![[10.0, 110.0, 90.0, 110.0, 10.0, 100.0, 90.0, 100.0]],
        overlay: String::new(),
        look: pdfcraft_annot::OverlayLook::default(),
    };
    add(&mut src, 0, shape, "", "red");
    let x = export_annotation(&src, "red").unwrap().xfdf.replace("flags=\"print\"", "flags=\"print,hidden,noview\"");
    assert!(x.contains("hidden"), "{x}");
    let mut dst = blank();
    assert_eq!(apply_remote(&mut dst, &[RemoteItem { annot_id: "red", author_id: "u", xfdf: &x }], &[]).failed, vec![]);
    let f = dst.get(find_by_name(&dst, "red").unwrap()).as_dict().unwrap().int(b"F").unwrap();
    assert_eq!(f & (1 | 2 | 32), 0, "flags {f}");
}

/// With `XFDF_DUMP_DIR` set, writes sample items (`<id>.json`: id, page, kind, xfdf) for checking
/// against the server's own parser (`packages/contracts/src/xfdf.ts`). Does nothing otherwise.
#[test]
fn dump_samples_for_the_server_parser() {
    let Ok(dir) = std::env::var("XFDF_DUMP_DIR") else { return };
    let mut src = blank();
    let note = add(&mut src, 0, Shape::Note { at: [100.0, 500.0], icon: NoteIcon::Comment }, "q", "note");
    add_reply(&mut src, 0, note, "a", "Bob", &meta("reply")).unwrap();
    set_review_state(&mut src, 0, note, pdfcraft_annot::ReviewState::Accepted, "Bob", &meta("status")).unwrap();
    let style = Style { color: [0.89, 0.13, 0.13], opacity: 1.0, width: 1.0, fill: None };
    let quads = [[100.0, 720.0, 200.0, 720.0, 100.0, 706.0, 200.0, 706.0]];
    add_text_replacement(&mut src, 1, &quads, "new", "Ada", &style, &style, &meta("strike"), &meta("caret")).unwrap();
    stamp(&mut src, 1, "stamp", Some(r#"{"templateId":"3f2b1c1e-8f7d-4c53-a0a5-0a0a0a0a0a0a","version":2,"values":{},"sig":"abc"}"#));
    let tb = add(&mut src, 0, Shape::TextBox { rect: [20.0, 200.0, 200.0, 260.0], font_size: 12.0 }, "서명 \"란\" <1>", "box");
    set_extra(&mut src, 0, tb, "PpmSignRole", "approver").unwrap();
    add(&mut src, 0, Shape::TextBox { rect: [20.0, 300.0, 200.0, 360.0], font_size: 12.0 }, "한글 상자", "ko");
    for id in ["note", "reply", "status", "strike", "caret", "stamp", "box", "ko"] {
        let it = export_annotation(&src, id).unwrap();
        let json = serde_json::json!({ "annotId": it.annot_id, "page": it.page, "kind": it.kind, "xfdf": it.xfdf });
        std::fs::write(std::path::Path::new(&dir).join(format!("{id}.json")), json.to_string()).unwrap();
    }
}
