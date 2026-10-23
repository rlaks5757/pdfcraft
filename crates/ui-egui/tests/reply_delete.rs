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
