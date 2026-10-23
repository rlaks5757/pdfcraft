//! Who may apply which edit, when the embedding page named an `actor` or made the session
//! read-only (see [`crate::Ownership`] and `pdfcraft_annot::ownership`). Every edit passes
//! [`check`] before it runs, whether on its own or inside a `Batch`.

use pdfcraft_annot::ownership as owner;

use crate::{Edit, EditError, Ownership};

/// Refuse `edit` on `doc` when the session is read-only, or when it would change a comment the
/// actor doesn't own. What an edit does to comments beyond its named target (page operations,
/// flattening, redaction, sanitizing) is held to the same rule afterwards, see
/// [`crate::run_edit_checked`].
pub(crate) fn check(doc: &pdfcraft_cos::Document, edit: &Edit, own: &Ownership) -> Result<(), EditError> {
    // A batch is only as read-only as the edits in it; an empty one changes nothing.
    if own.read_only && !matches!(edit, Edit::Batch { .. }) {
        return Err(EditError::HostReadOnly);
    }
    let Some(actor) = own.actor.as_deref() else { return Ok(()) };
    match edit {
        Edit::DeleteAnnotation { page, index } => owner::check_delete(doc, *page, *index, actor)?,
        // Replying is how anyone takes part in someone else's comment; setting its status or
        // marking it changes it, so those are the owner's.
        Edit::SetAnnotationContents { page, index, .. }
        | Edit::SetAnnotationStatus { page, index, .. }
        | Edit::MarkAnnotation { page, index, .. }
        | Edit::LockAnnotation { page, index, .. }
        | Edit::MoveAnnotation { page, index, .. }
        | Edit::ResizeAnnotation { page, index, .. }
        | Edit::StyleAnnotation { page, index, .. }
        | Edit::FillAnnotation { page, index, .. }
        | Edit::FontSizeAnnotation { page, index, .. }
        | Edit::SetAnnotationInfo { page, index, .. }
        | Edit::EraseInk { page, index, .. } => owner::check_target(doc, *page, *index, actor)?,
        // Pages go with their comments; a copy of someone else's comment would be a new comment
        // that says it is theirs.
        Edit::DeletePages { pages } | Edit::DuplicatePages { pages, .. } => owner::check_pages(doc, pages, actor)?,
        Edit::Flatten { comments: true, .. } => owner::check_all_pages(doc, actor)?,
        _ => {}
    }
    Ok(())
}

/// Edits that can bring new comments into the document (their own, copied pages' or imported).
/// Each comment they add is marked as the actor's right after the edit.
pub(crate) fn creates_comments(edit: &Edit) -> bool {
    matches!(
        edit,
        Edit::AddMeasurement(_)
            | Edit::AddAnnotation(_)
            | Edit::AddCustomStamp { .. }
            | Edit::ReplyToAnnotation { .. }
            | Edit::SetAnnotationStatus { .. }
            | Edit::MarkAnnotation { .. }
            | Edit::ReplaceText { .. }
            | Edit::EraseInk { .. }
            | Edit::ImportData { .. }
            | Edit::InsertPagesFrom { .. }
            | Edit::DuplicatePages { .. }
            | Edit::ReplacePages { .. }
    )
}
