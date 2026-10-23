//! Deleting a reply keeps its comment selected; deleting the comment clears the selection.

use pdfcraft_engine::{Edit, NewAnnotation, NoteIcon, Session, Shape, Style};
use pdfcraft_ui_egui::PdfCraftApp;

fn note(page: usize) -> Edit {
    let style = Style { color: [1.0, 0.8, 0.0], opacity: 1.0, width: 1.0, fill: None };
    Edit::AddAnnotation(NewAnnotation { page, shape: Shape::Note { at: [50.0, 50.0], icon: NoteIcon::Comment }, style, contents: "n".into(), author: "A".into() })
}

fn app_with_thread() -> PdfCraftApp {
    let mut app = PdfCraftApp::new();
    app.session = std::mem::take(&mut app.session).with_clock(|| 1_700_000_000);
    let bytes = Session::new().create_blank(200.0, 300.0, 1).unwrap().to_vec();
    app.open_bytes("t.pdf", None, bytes).unwrap();
    assert!(app.apply_edit(note(0)));
    assert!(app.apply_edit(note(0)));
    // Two replies to the first comment.
    for text in ["one", "two"] {
        assert!(app.apply_edit(Edit::ReplyToAnnotation { page: 0, index: 0, text: text.into(), author: "B".into() }));
    }
    app
}

fn reply_index(app: &PdfCraftApp, text: &str) -> usize {
    let doc = app.session.get(app.views[0].id).unwrap();
    doc.info.annotations.iter().find(|a| a.in_reply_to.is_some() && a.contents.as_deref() == Some(text)).unwrap().index
}

#[test]
fn deleting_a_reply_keeps_its_comment_selected() {
    let mut app = app_with_thread();
    app.views[0].comments.selected = Some((0, 0));
    let index = reply_index(&app, "one");
    assert!(app.apply_edit(Edit::DeleteAnnotation { page: 0, index }));
    assert_eq!(app.views[0].comments.selected, Some((0, 0)));
    let doc = app.session.get(app.views[0].id).unwrap();
    assert_eq!(doc.info.annotations.iter().filter(|a| a.in_reply_to.is_some()).count(), 1, "only that reply went");
}

#[test]
fn deleting_the_comment_clears_the_selection() {
    let mut app = app_with_thread();
    app.views[0].comments.selected = Some((0, 1));
    assert!(app.apply_edit(Edit::DeleteAnnotation { page: 0, index: 1 }));
    assert_eq!(app.views[0].comments.selected, None);
}

/// A page with three comments: two notes that share one `/NM` (files made by sessions that
/// started from the same state do), and a reply to the first.
fn shared_nm_pdf() -> Vec<u8> {
    let note = |extra: &str| format!("<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /NM (same) /T (A) /Contents (note) {extra} >>");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R 6 0 R] >>".to_string(),
        note(""),
        note(""),
        "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /NM (other) /T (B) /Contents (the reply) /IRT 4 0 R >>".to_string(),
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

#[test]
fn a_reply_belongs_to_the_comment_it_points_at_even_when_names_collide() {
    let mut app = PdfCraftApp::new();
    app.open_bytes("t.pdf", None, shared_nm_pdf()).unwrap();
    let doc = app.session.get(app.views[0].id).unwrap();
    let a = &doc.info.annotations;
    let find = |i: usize| a.iter().find(|x| x.index == i).unwrap();
    let (first, second, reply) = (find(0), find(1), find(2));
    assert_eq!(first.name, second.name, "the two comments share a name");
    assert!(reply.replies_to(first));
    assert!(!reply.replies_to(second), "the reply is not shown under the other comment");
}

#[test]
fn deleting_a_reply_keeps_the_right_comment_selected_when_names_collide() {
    let mut app = PdfCraftApp::new();
    app.open_bytes("t.pdf", None, shared_nm_pdf()).unwrap();
    app.views[0].comments.selected = Some((0, 1));
    assert!(app.apply_edit(Edit::DeleteAnnotation { page: 0, index: 2 }));
    assert_eq!(app.views[0].comments.selected, Some((0, 0)), "the one the reply was to");
}
