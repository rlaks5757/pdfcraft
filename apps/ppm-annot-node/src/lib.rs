//! The PPM server's view of a PDF's annotations, for Node (`wasm-bindgen --target nodejs`).
//!
//! Five functions, bytes as `Uint8Array` and everything else as JSON text. Pages are 1-based
//! everywhere here (as in the list); `index` is the entry's place in that page's `/Annots` array,
//! every subtype counted, so annotations without `/NM` are addressed the same way.
//!
//! * `list(pdf)` → `[{page, index, subtype, nm, title, irt: {page, index} | null, exportable}]`
//! * `exportXfdf(pdf, [{page, index, name, inReplyTo}])` → `[{name, xfdf} | {name, error}]`
//! * `strip(pdf, [{page, index}])` → PDF without those annotations and their pop-ups
//! * `merge(pdf, [{annotId, authorId, xfdf}])` → PDF with them added, appearances drawn
//! * `flatten(pdf)` → PDF with every comment and form field baked into the pages
//!
//! The logic lives in plain functions ([`list_json`] …) so that it is testable on the host; the
//! `#[wasm_bindgen]` functions only convert errors.

use std::sync::Arc;

use pdfcraft_cos::{Document, SaveOptions, write_full};
use pdfcraft_edit::{EditError, flatten_export};
use pdfcraft_xfdf::{ExportRequest, RemoteItem, apply_remote, export_positions, list_annotations, strip_positions};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

fn open(pdf: Vec<u8>) -> Result<Document, String> {
    Document::open(Arc::new(pdf)).map_err(|e| format!("the PDF can't be opened: {e}"))
}

fn save(doc: &Document) -> Result<Vec<u8>, String> {
    write_full(doc, &SaveOptions::default()).map_err(|e| format!("the PDF can't be saved: {e}"))
}

#[derive(Serialize)]
struct Position {
    page: usize,
    index: usize,
}

#[derive(Serialize)]
struct ListEntry {
    page: usize,
    index: usize,
    subtype: String,
    nm: Option<String>,
    title: Option<String>,
    irt: Option<Position>,
    exportable: bool,
}

pub fn list_json(pdf: Vec<u8>) -> Result<String, String> {
    let doc = open(pdf)?;
    let list: Vec<ListEntry> = list_annotations(&doc)
        .into_iter()
        .map(|e| ListEntry {
            page: e.page + 1,
            index: e.index,
            subtype: e.subtype,
            nm: e.nm,
            title: e.title,
            irt: e.irt.map(|(page, index)| Position { page: page + 1, index }),
            exportable: e.exportable,
        })
        .collect();
    serde_json::to_string(&list).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportIn {
    page: usize,
    index: usize,
    name: String,
    #[serde(default)]
    in_reply_to: Option<String>,
}

#[derive(Serialize)]
struct ExportOut {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    xfdf: Option<String>,
    /// Why this one could not be exported (the rest still are).
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// 1-based page → zero-based; 0 stays out of range so it is reported as missing.
fn page0(page: usize) -> usize {
    page.wrapping_sub(1)
}

pub fn export_json(pdf: Vec<u8>, requests: &str) -> Result<String, String> {
    let input: Vec<ExportIn> = serde_json::from_str(requests).map_err(|e| format!("bad export request: {e}"))?;
    let doc = open(pdf)?;
    let reqs: Vec<ExportRequest> =
        input.iter().map(|r| ExportRequest { page: page0(r.page), index: r.index, name: &r.name, in_reply_to: r.in_reply_to.as_deref() }).collect();
    let out: Vec<ExportOut> = export_positions(&doc, &reqs)
        .into_iter()
        .zip(&input)
        .map(|(res, r)| match res {
            Ok(item) => ExportOut { name: r.name.clone(), xfdf: Some(item.xfdf), error: None },
            Err(e) => ExportOut { name: r.name.clone(), xfdf: None, error: Some(e.to_string()) },
        })
        .collect();
    serde_json::to_string(&out).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
struct StripIn {
    page: usize,
    index: usize,
}

pub fn strip_pdf(pdf: Vec<u8>, targets: &str) -> Result<Vec<u8>, String> {
    let input: Vec<StripIn> = serde_json::from_str(targets).map_err(|e| format!("bad strip request: {e}"))?;
    if input.is_empty() {
        return Ok(pdf);
    }
    let mut doc = open(pdf)?;
    let at: Vec<(usize, usize)> = input.iter().map(|t| (page0(t.page), t.index)).collect();
    strip_positions(&mut doc, &at).map_err(|e| e.to_string())?;
    save(&doc)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergeIn {
    annot_id: String,
    author_id: String,
    xfdf: String,
}

pub fn merge_pdf(pdf: Vec<u8>, items: &str) -> Result<Vec<u8>, String> {
    let input: Vec<MergeIn> = serde_json::from_str(items).map_err(|e| format!("bad merge request: {e}"))?;
    if input.is_empty() {
        return Ok(pdf);
    }
    let mut doc = open(pdf)?;
    let remote: Vec<RemoteItem> = input.iter().map(|i| RemoteItem { annot_id: &i.annot_id, author_id: &i.author_id, xfdf: &i.xfdf }).collect();
    let report = apply_remote(&mut doc, &remote, &[]);
    // Leaving an annotation out silently would lose it from the export: fail with every reason.
    if !report.failed.is_empty() {
        let why: Vec<String> = report.failed.iter().map(|(id, reason)| format!("{id}: {reason}")).collect();
        return Err(format!("annotations could not be merged: {}", why.join("; ")));
    }
    save(&doc)
}

/// Prefix of the error when flattening is refused because the document has too many annotations
/// (nothing is changed then); the caller tells this apart from a broken PDF by it.
pub const FLATTEN_REFUSED: &str = "flatten refused: ";

/// Bakes every comment and form field into the page content, as the /viewer export does with
/// `FlattenAnnotations` (NoView annotations and link borders included: `flatten_export`). This goes to the crate function, not through the engine: the engine's
/// ownership guard protects other people's comments from a user's edit, while this runs on the
/// server because the administrator set the integration to flatten its exports, whoever wrote them.
pub fn flatten_pdf(pdf: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut doc = open(pdf)?;
    let pages: Vec<usize> = (0..pdfcraft_model::pages(&doc).len()).collect();
    match flatten_export(&mut doc, &pages) {
        Ok(_) => {}
        // The crate refuses a document over its annotation limit before changing anything.
        Err(EditError::Invalid(why)) => return Err(format!("{FLATTEN_REFUSED}{why}")),
        Err(e) => return Err(format!("the PDF can't be flattened: {e}")),
    }
    save(&doc)
}

#[wasm_bindgen]
pub fn list(pdf: Vec<u8>) -> Result<String, JsError> {
    list_json(pdf).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = exportXfdf)]
pub fn export_xfdf(pdf: Vec<u8>, requests: &str) -> Result<String, JsError> {
    export_json(pdf, requests).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen]
pub fn strip(pdf: Vec<u8>, targets: &str) -> Result<Vec<u8>, JsError> {
    strip_pdf(pdf, targets).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen]
pub fn merge(pdf: Vec<u8>, items: &str) -> Result<Vec<u8>, JsError> {
    merge_pdf(pdf, items).map_err(|e| JsError::new(&e))
}

#[wasm_bindgen(js_name = flatten)]
pub fn flatten_bytes(pdf: Vec<u8>) -> Result<Vec<u8>, JsError> {
    flatten_pdf(pdf).map_err(|e| JsError::new(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        assert!(list_json(b"not a pdf".to_vec()).is_err());
        assert!(export_json(Vec::new(), "[]").is_err());
        assert!(strip_pdf(b"x".to_vec(), "not json").unwrap_err().contains("bad strip request"));
        assert!(merge_pdf(b"x".to_vec(), "{}").unwrap_err().contains("bad merge request"));
        assert!(flatten_pdf(b"not a pdf".to_vec()).is_err());
    }

    #[test]
    fn nothing_to_do_returns_the_input() {
        assert_eq!(strip_pdf(b"anything".to_vec(), "[]").unwrap(), b"anything");
        assert_eq!(merge_pdf(b"anything".to_vec(), "[]").unwrap(), b"anything");
    }

    /// A one-page PDF whose /Annots hold a link and a square without an appearance.
    fn sample() -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Annots [4 0 R 5 0 R] >>",
            "<< /Type /Annot /Subtype /Link /Rect [10 10 50 30] /Border [0 0 0] >>",
            "<< /Type /Annot /Subtype /Square /Rect [200 200 300 260] /C [1 0 0] /F 4 /BS << /W 2 >> >>",
        ];
        let mut out = String::from("%PDF-1.7\n");
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out += &format!("{} 0 obj\n{o}\nendobj\n", i + 1);
        }
        let xref = out.len();
        out += &format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1);
        for o in offs {
            out += &format!("{o:010} 00000 n \n");
        }
        out += &format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1);
        out.into_bytes()
    }

    #[test]
    fn flatten_bakes_comments_and_keeps_links() {
        let out = flatten_pdf(sample()).unwrap();
        let list: Vec<serde_json::Value> = serde_json::from_str(&list_json(out).unwrap()).unwrap();
        let kinds: Vec<&str> = list.iter().filter_map(|e| e["subtype"].as_str()).collect();
        assert_eq!(kinds, ["Link"]);
    }
}
