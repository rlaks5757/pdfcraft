//! PdfCraft in the browser. Build: `cd apps/pdfcraft-web && trunk build --release`
//! (trunk generates the JS loader; no handwritten JS — plan/execution-plan.md §1).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_arch = "wasm32")]
fn main() {
    use eframe::wasm_bindgen::JsCast;
    use pdfcraft_ui_egui::PdfCraftApp;

    let options = eframe::WebOptions { renderer: eframe::Renderer::Glow, ..Default::default() };
    wasm_bindgen_futures::spawn_local(async move {
        let Some(canvas) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("pdfcraft"))
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        else {
            return;
        };
        let started = eframe::WebRunner::new()
            .start(
                canvas,
                options,
                Box::new(|cc| {
                    let mut app = PdfCraftApp::new();
                    // "printcraft": settings saved under the app's former name, PrintCraft.
                    if let Some(json) = cc.storage.and_then(|s| s.get_string("pdfcraft").or_else(|| s.get_string("printcraft"))) {
                        app.restore(&json);
                    }
                    // `?file=<url>` opens a PDF from a URL (same-origin or CORS-enabled).
                    if let Some(url) = query_param("file") {
                        let inbox = app.startup_inbox.clone();
                        let failed = app.failed_inbox.clone();
                        let ctx = cc.egui_ctx.clone();
                        wasm_bindgen_futures::spawn_local(async move {
                            let name = url.rsplit('/').next().unwrap_or("document.pdf").split('?').next().unwrap_or("document.pdf").to_string();
                            match fetch_bytes(&url).await {
                                Ok(bytes) => {
                                    if let Ok(mut q) = inbox.lock() {
                                        q.push((name, bytes));
                                    }
                                }
                                Err(e) => {
                                    eframe::web_sys::console::error_1(&format!("PdfCraft: could not fetch {url}: {e}").into());
                                    // Shown in the app too, not only in the console (#173).
                                    if let Ok(mut q) = failed.lock() {
                                        q.push((name, e));
                                    }
                                }
                            }
                            ctx.request_repaint();
                        });
                    }
                    // eframe never raises a close request on a browser reload or tab close, so `guard_quit` never
                    // prompts; ask the browser to confirm instead while a document has unsaved work (#812).
                    warn_before_unload(app.unsaved_flag.clone());
                    // `?host=parent`: a same-origin page embedding PdfCraft in an iframe (such
                    // as the Nextcloud app) passes files in and takes saves back (`host` below).
                    if query_param("host").as_deref() == Some("parent")
                        && let Some(bridge) = host::Bridge::new()
                    {
                        // `?author=`: the host's name for the user signs their comments,
                        // unless they chose a name in PdfCraft (the web default is "Guest").
                        if let Some(author) = query_param("author").map(|a| a.trim().to_string()).filter(|a| !a.is_empty())
                            && app.comment_prefs.author == "Guest"
                        {
                            app.comment_prefs.author = author.chars().take(200).collect();
                        }
                        bridge.listen(app.startup_inbox.clone(), app.stamp_inbox.clone(), app.compare_inbox.clone(), app.ownership_inbox.clone(), cc.egui_ctx.clone());
                        app.host_stamp = Some(bridge.stamp_fn());
                        app.host_compare = Some(bridge.compare_fn());
                        app.host_save = Some(bridge.save_fn());
                        app.host_dirty = Some(bridge.dirty_fn());
                        bridge.post_ready();
                    } else if query_param("host").as_deref() == Some("parent") {
                        eframe::web_sys::console::warn_1(
                            &"PdfCraft: ?host=parent needs a framing page on the same origin; saves download instead".into(),
                        );
                    }
                    Ok(Box::new(app))
                }),
            )
            .await;
        if let Err(e) = started {
            eframe::web_sys::console::error_1(&e);
            if query_param("host").as_deref() == Some("parent")
                && let Some(bridge) = host::Bridge::new()
            {
                bridge.post_failed(&e.as_string().unwrap_or_else(|| format!("{e:?}")));
            }
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn warn_before_unload(unsaved: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use eframe::wasm_bindgen::{JsCast, closure::Closure};
    let Some(window) = web_sys::window() else { return };
    let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        if unsaved.load(std::sync::atomic::Ordering::Relaxed) {
            event.prevent_default();
        }
    });
    if window.add_event_listener_with_callback("beforeunload", handler.as_ref().unchecked_ref()).is_ok() {
        handler.forget();
    }
}

#[cfg(target_arch = "wasm32")]
fn query_param(key: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&search).ok()?.get(key)
}

#[cfg(target_arch = "wasm32")]
async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    use eframe::wasm_bindgen::JsCast;
    let window = web_sys::window().ok_or("no window")?;
    let resp = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url)).await.map_err(|e| format!("{e:?}"))?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| "not a response")?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer().map_err(|e| format!("{e:?}"))?).await.map_err(|e| format!("{e:?}"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// The `postMessage` bridge to a same-origin parent page. Messages are plain objects with a
/// `type`:
///
/// - `pdfcraft:ready` (to the parent): the app is listening; send files now.
/// - `pdfcraft:open` (from the parent): `{ name, bytes, actor?, readOnly? }`, where `bytes` is an
///   `ArrayBuffer` or `Uint8Array` holding a PDF. It opens in a tab, like a `?file=` URL.
///   `actor` (a string of 1 to 200 characters, the user's id) makes every comment the app creates
///   the user's, and lets only that user change or delete comments; comments without an owner are
///   locked. `readOnly` (a boolean) refuses every edit that changes the document. Both default
///   to off, and each open sets them anew. A wrong type or length refuses the open with
///   `pdfcraft:failed` instead of opening without the checks.
/// - `pdfcraft:save` (to the parent): `{ name, saveAs, bytes }` when the user saves; `bytes` is
///   a transferred `ArrayBuffer`. The parent stores the file (and reports its own errors).
/// - `pdfcraft:stamp-add` (from the parent): `{ id, name, bytes, width, height, meta }` arms one
///   placement of a PNG stamp, `width` and `height` in PDF points, `meta` the text stored as the
///   stamp's `/PpmStamp`. `pdfcraft:stamp-cancel` `{ id }` withdraws it.
/// - `pdfcraft:stamp-armed` `{ id }`, `pdfcraft:stamp-placed` `{ id, page }`,
///   `pdfcraft:stamp-cancelled` `{ id }` and `pdfcraft:stamp-failed` `{ id, error }` (to the
///   parent) report how a stamp request went.
/// - `pdfcraft:compare` (from the parent): `{ id, old: { name, bytes }, new: { name, bytes } }`
///   opens the old file in a background tab and the new file as the active one, then runs the
///   Compare Files comparison. A window compares once; `pdfcraft:compare-done`
///   `{ id, replaced, inserted, deleted, visual }` or `pdfcraft:compare-failed` `{ id, error }`
///   (to the parent) report the outcome. In this mode the Compare panel has no Mark as
///   comments or Report.
/// - `pdfcraft:dirty` (to the parent): `{ dirty }` whenever unsaved work appears or goes away.
/// - `pdfcraft:failed` (to the parent): `{ error }` when the app couldn't start.
///
/// Only the parent window on the app's own origin is heard or answered. With a parent on
/// another origin there is no bridge: the app runs as if unframed and saves download.
#[cfg(target_arch = "wasm32")]
mod host {
    use eframe::wasm_bindgen::{JsCast, JsValue, closure::Closure};

    #[derive(Clone)]
    pub struct Bridge {
        window: web_sys::Window,
        parent: web_sys::Window,
        origin: String,
    }

    impl Bridge {
        /// `None` when the page isn't framed (there is no parent to talk to), or when the parent
        /// is on another origin: messages only go to this origin, so the browser would drop
        /// every save without an error while the document showed as saved (#2167).
        pub fn new() -> Option<Self> {
            let window = web_sys::window()?;
            let parent = window.parent().ok().flatten()?;
            if js_sys::Object::is(&parent, &window) {
                return None;
            }
            let origin = window.location().origin().ok()?;
            // Reading another origin's location throws, which is `None` too.
            if parent.location().origin().ok()? != origin {
                return None;
            }
            Some(Self { window, parent, origin })
        }

        fn post(&self, message: &js_sys::Object, transfer: Option<&JsValue>) -> Result<(), String> {
            let sent = match transfer {
                Some(t) => self.parent.post_message_with_transfer(message, &self.origin, &js_sys::Array::of1(t)),
                None => self.parent.post_message(message, &self.origin),
            };
            sent.map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))
        }

        pub fn post_ready(&self) {
            if let Some(m) = message(&[("type", "pdfcraft:ready".into()), ("version", env!("CARGO_PKG_VERSION").into())]) {
                let _ = self.post(&m, None);
            }
        }

        /// The app couldn't start (no WebGL, for example): let the parent say so.
        pub fn post_failed(&self, error: &str) {
            if let Some(m) = message(&[("type", "pdfcraft:failed".into()), ("error", error.into())]) {
                let _ = self.post(&m, None);
            }
        }

        /// Hear `pdfcraft:open` from the parent and queue the file like a `?file=` download.
        pub fn listen(
            &self,
            inbox: pdfcraft_ui_egui::Inbox,
            stamps: pdfcraft_ui_egui::StampInbox,
            compares: pdfcraft_ui_egui::CompareInbox,
            ownership: pdfcraft_ui_egui::OwnershipInbox,
            ctx: egui::Context,
        ) {
            let bridge = self.clone();
            let parent = self.parent.clone();
            let origin = self.origin.clone();
            let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
                if event.origin() != origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
                    return;
                }
                let data = event.data();
                match string(&data, "type").as_deref() {
                    Some("pdfcraft:open") => {}
                    Some("pdfcraft:stamp-add") => {
                        let id = string(&data, "id").unwrap_or_default();
                        match stamp_add(&data) {
                            Ok(command) => {
                                if let Ok(mut q) = stamps.lock() {
                                    q.push(command);
                                }
                                ctx.request_repaint();
                            }
                            Err(error) => bridge.post_stamp(&pdfcraft_ui_egui::StampEvent::Failed { id, error }),
                        }
                        return;
                    }
                    Some("pdfcraft:compare") => {
                        let id = string(&data, "id").unwrap_or_default();
                        match compare_request(&data) {
                            Ok(command) => {
                                if let Ok(mut q) = compares.lock() {
                                    q.push(command);
                                }
                                ctx.request_repaint();
                            }
                            Err(error) => bridge.post_compare(&pdfcraft_ui_egui::CompareEvent::Failed { id: id.chars().take(128).collect(), error }),
                        }
                        return;
                    }
                    Some("pdfcraft:stamp-cancel") => {
                        if let Some(id) = string(&data, "id") {
                            if let Ok(mut q) = stamps.lock() {
                                q.push(pdfcraft_ui_egui::StampCommand::Cancel { id });
                            }
                            ctx.request_repaint();
                        }
                        return;
                    }
                    _ => return,
                }
                let name = string(&data, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "document.pdf".to_string());
                let who = match open_ownership(&data) {
                    Ok(who) => who,
                    Err(error) => {
                        bridge.post_failed(&error);
                        return;
                    }
                };
                let Ok(bytes) = js_sys::Reflect::get(&data, &JsValue::from_str("bytes")) else { return };
                let bytes = if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
                    array.to_vec()
                } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
                    js_sys::Uint8Array::new(&bytes).to_vec()
                } else {
                    return;
                };
                // The rules go first, so the file is never opened without them.
                if let Ok(mut o) = ownership.lock() {
                    *o = Some(who);
                }
                if let Ok(mut q) = inbox.lock() {
                    q.push((name, bytes));
                }
                ctx.request_repaint();
            });
            if self.window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
                // The listener lives as long as the page.
                on_message.forget();
            }
        }

        /// Tell the parent how a stamp request went.
        pub fn post_stamp(&self, event: &pdfcraft_ui_egui::StampEvent) {
            use pdfcraft_ui_egui::StampEvent as E;
            let fields = match event {
                E::Armed { id } => vec![("type", "pdfcraft:stamp-armed".into()), ("id", id.as_str().into())],
                E::Placed { id, page } => {
                    vec![("type", "pdfcraft:stamp-placed".into()), ("id", id.as_str().into()), ("page", JsValue::from_f64(*page as f64))]
                }
                E::Cancelled { id } => vec![("type", "pdfcraft:stamp-cancelled".into()), ("id", id.as_str().into())],
                E::Failed { id, error } => {
                    vec![("type", "pdfcraft:stamp-failed".into()), ("id", id.as_str().into()), ("error", error.as_str().into())]
                }
            };
            if let Some(m) = message(&fields) {
                let _ = self.post(&m, None);
            }
        }

        /// Tell the parent how a compare request went.
        pub fn post_compare(&self, event: &pdfcraft_ui_egui::CompareEvent) {
            use pdfcraft_ui_egui::CompareEvent as E;
            let count = |n: usize| JsValue::from_f64(n as f64);
            let fields = match event {
                E::Done { id, replaced, inserted, deleted, visual } => vec![
                    ("type", "pdfcraft:compare-done".into()),
                    ("id", id.as_str().into()),
                    ("replaced", count(*replaced)),
                    ("inserted", count(*inserted)),
                    ("deleted", count(*deleted)),
                    ("visual", count(*visual)),
                ],
                E::Failed { id, error } => {
                    vec![("type", "pdfcraft:compare-failed".into()), ("id", id.as_str().into()), ("error", error.as_str().into())]
                }
            };
            if let Some(m) = message(&fields) {
                let _ = self.post(&m, None);
            }
        }

        pub fn compare_fn(&self) -> pdfcraft_ui_egui::HostCompareFn {
            let bridge = self.clone();
            Box::new(move |event| bridge.post_compare(event))
        }

        pub fn stamp_fn(&self) -> pdfcraft_ui_egui::HostStampFn {
            let bridge = self.clone();
            Box::new(move |event| bridge.post_stamp(event))
        }

        pub fn save_fn(&self) -> pdfcraft_ui_egui::HostSaveFn {
            let bridge = self.clone();
            Box::new(move |name: &str, bytes: &[u8], save_as: bool| {
                let buffer = js_sys::Uint8Array::from(bytes).buffer();
                let m =
                    message(&[("type", "pdfcraft:save".into()), ("name", name.into()), ("saveAs", save_as.into()), ("bytes", buffer.clone().into())])
                        .ok_or_else(|| "couldn't build the message".to_string())?;
                bridge.post(&m, Some(&buffer))
            })
        }

        pub fn dirty_fn(&self) -> pdfcraft_ui_egui::HostDirtyFn {
            let bridge = self.clone();
            let mut last = false;
            Box::new(move |dirty: bool| {
                if dirty != last {
                    last = dirty;
                    if let Some(m) = message(&[("type", "pdfcraft:dirty".into()), ("dirty", dirty.into())]) {
                        let _ = bridge.post(&m, None);
                    }
                }
            })
        }
    }

    fn message(fields: &[(&str, JsValue)]) -> Option<js_sys::Object> {
        let m = js_sys::Object::new();
        for (key, value) in fields {
            js_sys::Reflect::set(&m, &JsValue::from_str(key), value).ok()?;
        }
        Some(m)
    }

    /// Read one `{ name, bytes }` file of a `pdfcraft:compare` message.
    fn compare_file(data: &JsValue, key: &str) -> Result<(String, Vec<u8>), String> {
        let file = js_sys::Reflect::get(data, &JsValue::from_str(key)).map_err(|_| format!("{key} is missing"))?;
        if !file.is_object() {
            return Err(format!("{key} must be an object"));
        }
        let name = string(&file, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "document.pdf".to_string());
        let bytes = js_sys::Reflect::get(&file, &JsValue::from_str("bytes")).unwrap_or(JsValue::UNDEFINED);
        let array = if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
            array.clone()
        } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
            js_sys::Uint8Array::new(&bytes)
        } else {
            return Err(format!("{key}.bytes must be a Uint8Array or ArrayBuffer"));
        };
        // Look at the length before copying anything into wasm memory.
        if array.length() as usize > pdfcraft_ui_egui::MAX_COMPARE_BYTES {
            return Err(format!("{key} is too large (at most {} MB)", pdfcraft_ui_egui::MAX_COMPARE_BYTES >> 20));
        }
        Ok((name.chars().take(200).collect(), array.to_vec()))
    }

    /// Read a `pdfcraft:compare` message; the error is what the parent is told.
    fn compare_request(data: &JsValue) -> Result<pdfcraft_ui_egui::CompareCommand, String> {
        let id = string(data, "id").ok_or_else(|| "id must be a string".to_string())?;
        Ok(pdfcraft_ui_egui::CompareCommand { id, old: compare_file(data, "old")?, new: compare_file(data, "new")? })
    }

    /// Read a `pdfcraft:stamp-add` message; the error is what the parent is told.
    fn stamp_add(data: &JsValue) -> Result<pdfcraft_ui_egui::StampCommand, String> {
        let get = |key: &str| js_sys::Reflect::get(data, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED);
        let text = |key: &str| get(key).as_string().ok_or_else(|| format!("{key} must be a string"));
        let number = |key: &str| get(key).as_f64().ok_or_else(|| format!("{key} must be a number"));
        let bytes = get("bytes");
        let array = if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
            array.clone()
        } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
            js_sys::Uint8Array::new(&bytes)
        } else {
            return Err("bytes must be a Uint8Array or ArrayBuffer".into());
        };
        // Look at the length before copying anything into wasm memory.
        if array.length() as usize > pdfcraft_ui_egui::MAX_PNG_BYTES {
            return Err(format!("the stamp is too large (at most {} MB)", pdfcraft_ui_egui::MAX_PNG_BYTES >> 20));
        }
        let bytes = array.to_vec();
        Ok(pdfcraft_ui_egui::StampCommand::Add {
            id: text("id")?,
            name: text("name")?,
            bytes,
            width: number("width")?,
            height: number("height")?,
            meta: text("meta")?,
        })
    }

    /// Read `actor` and `readOnly` of a `pdfcraft:open` message. Missing means off; a wrong type or
    /// length is an error (the open is refused), never a way around the checks.
    fn open_ownership(data: &JsValue) -> Result<pdfcraft_ui_egui::Ownership, String> {
        let get = |key: &str| js_sys::Reflect::get(data, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED);
        let (actor, read_only) = (get("actor"), get("readOnly"));
        let actor = if actor.is_undefined() || actor.is_null() {
            None
        } else {
            let a = actor.as_string().filter(|a| (1..=200).contains(&a.chars().count()));
            Some(a.ok_or_else(|| "actor must be a string of 1 to 200 characters".to_string())?)
        };
        let read_only = if read_only.is_undefined() || read_only.is_null() {
            false
        } else {
            read_only.as_bool().ok_or_else(|| "readOnly must be a boolean".to_string())?
        };
        Ok(pdfcraft_ui_egui::Ownership { actor, read_only })
    }

    fn string(data: &JsValue, key: &str) -> Option<String> {
        js_sys::Reflect::get(data, &JsValue::from_str(key)).ok()?.as_string()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("pdfcraft-web targets wasm32: run `trunk serve` or `trunk build --release` in apps/pdfcraft-web");
}
