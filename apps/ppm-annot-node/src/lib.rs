//! The PPM server's view of a PDF's annotations, for Node (`wasm-bindgen --target nodejs`).
//!
//! Four functions, bytes as `Uint8Array` and everything else as JSON text. Pages are 1-based
//! everywhere here (as in the list); `index` is the entry's place in that page's `/Annots` array,
//! every subtype counted, so annotations without `/NM` are addressed the same way.
//!
//! * `list(pdf)` → `[{page, index, subtype, nm, title, irt: {page, index} | null, exportable}]`
//! * `exportXfdf(pdf, [{page, index, name, inReplyTo}])` → `[{name, xfdf} | {name, error}]`
//! * `strip(pdf, [{page, index}])` → PDF without those annotations and their pop-ups
//! * `merge(pdf, [{annotId, authorId, xfdf}])` → PDF with them added, appearances drawn
//!
//! The logic lives in plain functions ([`list_json`] …) so that it is testable on the host; the
//! `#[wasm_bindgen]` functions only convert errors.

use std::sync::Arc;

use pdfcraft_cos::{Document, SaveOptions, write_full};
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        assert!(list_json(b"not a pdf".to_vec()).is_err());
        assert!(export_json(Vec::new(), "[]").is_err());
        assert!(strip_pdf(b"x".to_vec(), "not json").unwrap_err().contains("bad strip request"));
        assert!(merge_pdf(b"x".to_vec(), "{}").unwrap_err().contains("bad merge request"));
    }

    #[test]
    fn nothing_to_do_returns_the_input() {
        assert_eq!(strip_pdf(b"anything".to_vec(), "[]").unwrap(), b"anything");
        assert_eq!(merge_pdf(b"anything".to_vec(), "[]").unwrap(), b"anything");
    }
}
