use pdfcraft_annot::{Meta, NewAnnotation, NoteIcon, Shape, Style, add_annotation, add_reply};

use super::*;
use crate::single::annots_of;
use crate::tests::blank;

fn meta(id: &str) -> Meta {
    Meta { date: Some("D:20261023120000+09'00'".into()), id: id.into() }
}

fn add(doc: &mut Document, page: usize, shape: Shape, contents: &str, id: &str) -> usize {
    let style = Style::default_for(&shape);
    add_annotation(doc, &NewAnnotation { page, shape, style, contents: contents.into(), author: "Ada".into() }, &meta(id)).unwrap()
}

/// Page 0 `/Annots`: [link, note (no /NM), reply (no /NM), square (no /NM)].
fn file_made_elsewhere() -> Document {
    let mut doc = blank();
    let page = pdfcraft_model::pages(&doc)[0].obj;
    let mut link = Dict::new();
    link.set(b"Type".to_vec(), Object::name("Annot"));
    link.set(b"Subtype".to_vec(), Object::name("Link"));
    link.set(b"Rect".to_vec(), Object::Array(vec![0.into(), 0.into(), 10.into(), 10.into()]));
    let link = doc.add(link);
    doc.update_dict(page, |d| d.set(b"Annots".to_vec(), Object::Array(vec![Object::Ref(link)]))).unwrap();
    let note = add(&mut doc, 0, Shape::Note { at: [100.0, 500.0], icon: NoteIcon::Comment }, "질문입니다", "n");
    add_reply(&mut doc, 0, note, "답변", "Bob", &meta("r")).unwrap();
    add(&mut doc, 0, Shape::Rectangle { rect: [50.0, 50.0, 150.0, 120.0] }, "", "s");
    // The file never named them.
    for e in annots_of(&doc, page) {
        if let Some(r) = e.as_ref() {
            doc.update_dict(r, |d| {
                d.remove(b"NM");
            })
            .unwrap();
        }
    }
    doc
}

/// Page 0 entries only, as (index, subtype).
fn page0(doc: &Document) -> Vec<AnnotEntry> {
    list_annotations(doc).into_iter().filter(|e| e.page == 0).collect()
}

fn index_of(list: &[AnnotEntry], subtype: &str, nth: usize) -> usize {
    list.iter().filter(|e| e.subtype == subtype).nth(nth).unwrap().index
}

#[test]
fn lists_every_entry_by_position_with_or_without_a_name() {
    let doc = file_made_elsewhere();
    let list = page0(&doc);
    // The link, a note with its pop-up, the reply, a square.
    let kinds: Vec<_> = list.iter().map(|e| (e.subtype.as_str(), e.exportable)).collect();
    assert_eq!(kinds[0], ("Link", false));
    assert!(list.iter().filter(|e| e.subtype == "Popup").all(|e| !e.exportable));
    assert!(list.iter().filter(|e| matches!(e.subtype.as_str(), "Text" | "Square")).all(|e| e.exportable));
    assert_eq!(list.iter().map(|e| e.index).collect::<Vec<_>>(), (0..list.len()).collect::<Vec<_>>());
    assert!(list.iter().all(|e| e.nm.is_none()));
    let (note, reply) = (index_of(&list, "Text", 0), index_of(&list, "Text", 1));
    assert_eq!(list[reply].irt, Some((0, note)));
    assert_eq!(list[note].title.as_deref(), Some("Ada"));
}

#[test]
fn exports_under_the_given_names_without_touching_the_file() {
    let doc = file_made_elsewhere();
    let list = page0(&doc);
    let (note, reply) = (index_of(&list, "Text", 0), index_of(&list, "Text", 1));
    let page = pdfcraft_model::pages(&doc)[0].obj;
    let before = doc.get(annots_of(&doc, page)[reply].as_ref().unwrap()).as_dict().cloned().unwrap();
    let reqs = [
        ExportRequest { page: 0, index: reply, name: "ppm-file-b", in_reply_to: Some("ppm-file-a") },
        ExportRequest { page: 0, index: note, name: "ppm-file-a", in_reply_to: None },
        ExportRequest { page: 0, index: 99, name: "x", in_reply_to: None },
    ];
    let out = export_positions(&doc, &reqs);
    let item = out[0].as_ref().unwrap();
    assert_eq!(item.annot_id, "ppm-file-b");
    assert!(
        item.xfdf.contains(r#"name="ppm-file-b""#) && item.xfdf.contains(r#"inreplyto="ppm-file-a""#) && item.xfdf.contains(r#"page="0""#),
        "{}",
        item.xfdf
    );
    assert!(out[1].as_ref().unwrap().xfdf.contains("질문입니다"));
    assert!(matches!(out[2], Err(DataError::NotFound(_))));
    let after = doc.get(annots_of(&doc, page)[reply].as_ref().unwrap()).as_dict().cloned().unwrap();
    assert_eq!(before, after, "the file's own dictionary must stay as it was (no /NM written)");
}

#[test]
fn an_exported_item_applies_back_to_a_clean_file() {
    let doc = file_made_elsewhere();
    let square = index_of(&page0(&doc), "Square", 0);
    let out = export_positions(&doc, &[ExportRequest { page: 0, index: square, name: "ppm-file-s", in_reply_to: None }]);
    let item = out[0].as_ref().unwrap();
    let mut dst = blank();
    let report = apply_remote(&mut dst, &[RemoteItem { annot_id: "ppm-file-s", author_id: "ppm:file", xfdf: &item.xfdf }], &[]);
    assert_eq!(report.failed, vec![]);
    assert_eq!(list_annotations(&dst).iter().filter(|e| e.nm.as_deref() == Some("ppm-file-s")).count(), 1);
}

#[test]
fn strip_removes_by_position_with_the_popup_and_keeps_the_rest_in_order() {
    let mut doc = file_made_elsewhere();
    let list = page0(&doc);
    let (note, square) = (index_of(&list, "Text", 0), index_of(&list, "Square", 0));
    // The note, its pop-up and the square go; the link and the reply stay.
    assert_eq!(strip_positions(&mut doc, &[(0, square), (0, note)]).unwrap(), 3);
    let left: Vec<_> = page0(&doc);
    assert_eq!(left.iter().map(|e| e.subtype.as_str()).collect::<Vec<_>>(), vec!["Link", "Text"]);
    // The reply is left without its parent: no longer exchangeable.
    assert!(!left[1].exportable);
}

#[test]
fn strip_with_a_missing_position_changes_nothing() {
    let mut doc = file_made_elsewhere();
    let n = list_annotations(&doc).len();
    assert!(matches!(strip_positions(&mut doc, &[(0, 1), (0, 99)]), Err(DataError::NotFound(_))));
    assert!(matches!(strip_positions(&mut doc, &[(5, 0)]), Err(DataError::NotFound(_))));
    assert_eq!(list_annotations(&doc).len(), n);
}
