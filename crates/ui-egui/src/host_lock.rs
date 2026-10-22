//! The host lock ("메뉴 막기"): a build made for an embedding page (cargo feature `host-lock`,
//! turned on by `ppm/build-web.sh`) opens only what that page sends, and does nothing that
//! reaches the user's own files, printers or the web.
//!
//! The lock is a compile-time value ([`HOST_LOCK`]); nothing at run time lifts it. It is held
//! where the work happens (opening, picking, dropping, downloading, opening links), so a
//! shortcut, a right-click or a drag is stopped the same as a menu item. The command list below
//! decides what the menus show and is checked for drift by a test: a command that is in none of
//! the lists fails it, and is denied until someone classifies it.

use std::sync::atomic::{AtomicBool, Ordering};

use pdfcraft_engine::{EditScope, Ownership, PolicyGroup};

/// Whether this build is made for an embedding page.
pub const HOST_LOCK: bool = cfg!(feature = "host-lock");

/// `pdfcraft:open` `mode`: `"builder"` lets everything not locked for every host through, and
/// anything else, including no mode or a value of another type, is the narrowest, `"viewer"`.
pub fn parse_mode(mode: Option<&str>) -> EditScope {
    match mode {
        Some("builder") => EditScope::All,
        _ => EditScope::Viewer,
    }
}

/// The edit scope to open a document with, given the `mode` the host sent. A build without the
/// lock has no modes: it edits as it always did.
pub fn open_scope(mode: Option<&str>) -> EditScope {
    if HOST_LOCK { parse_mode(mode) } else { EditScope::All }
}

/// Commands a viewer may run (and a builder too): comments, filling in, looking around.
pub(crate) const VIEWER: &[&str] = &[
    "ui.dock.reset",
    "file.save",
    "file.word_count",
    "edit.undo",
    "edit.redo",
    "edit.find",
    "edit.advanced_search",
    "edit.copy",
    "view.palette",
    "view.layout.single",
    "view.layout.two_up",
    "view.continuous",
    "view.layout.cover",
    "view.fit_width_scrolling",
    "view.fit_one_page",
    "view.fit_visible",
    "view.marquee_zoom",
    "view.full_screen",
    "view.read_mode",
    "view.next_tab",
    "view.previous_tab",
    "view.previous_view",
    "view.next_view",
    "view.focus_page_input",
    "view.theme",
    "view.theme.system",
    "view.theme.light",
    "view.theme.dark",
    "comment.list",
    "comment.note",
    "comment.freetext",
    "comment.highlight",
    "comment.underline",
    "comment.strikeout",
    "comment.squiggly",
    "comment.ink",
    "comment.line",
    "comment.arrow",
    "comment.square",
    "comment.circle",
    "comment.polygon",
    "comment.polyline",
    "comment.cloud",
    "comment.callout",
    "comment.caret",
    "comment.replace",
    "comment.eraser",
    "comment.hide_all",
    "form.fields",
    "form.clear",
    "sign.panel",
    "sign.validate",
    "help.shortcuts",
    "help.about",
];

/// Commands only a builder may run: editing the document itself.
pub(crate) const BUILDER: &[&str] = &[
    // Putting a file into the document is an edit, not a way out of it.
    "comment.attach",
    "comment.import",
    "form.import_data",
    "form.merge_data",
    "page.combine",
    "page.insert",
    "page.replace",
    "edit.image",
    "form.add.image",
    "doc.compare",
    "sign.fill.text",
    "sign.fill.check",
    "sign.fill.cross",
    "sign.fill.dot",
    "sign.fill.line",
    "sign.fill.date",
    "sign.fill.signature",
    "sign.fill.initials",
    "measure.distance",
    "measure.perimeter",
    "measure.area",
    "measure.scale",
    "measure.info",
    "measure.snap",
    "page.copy",
    "page.cut",
    "page.paste",
    "file.properties",
    "edit.column_select",
    "edit.snapshot",
    "comment.flatten",
    "comment.stamp",
    "comment.summarize",
    "form.flatten",
    "form.detect",
    "form.prepare",
    "form.tab_order.row",
    "form.tab_order.column",
    "form.tab_order.structure",
    "form.field.properties",
    "form.add.text",
    "form.add.checkbox",
    "form.add.radio",
    "form.add.combo",
    "form.add.list",
    "form.add.button",
    "form.add.date",
    "form.add.signature",
    "standards.pdfa",
    "edit.text",
    "edit.edit_text",
    "edit.link",
    "edit.links_from_urls",
    "edit.remove_links",
    "edit.bates",
    "redact.mark",
    "redact.pages",
    "redact.search",
    "redact.properties",
    "redact.apply",
    "redact.clear",
    "protect.remove_hidden",
    "redact.sanitize",
    "sign.digital",
    "sign.certify",
    "sign.certify_invisible",
    "sign.fill.signature.change",
    "sign.fill.signature.remove",
    "sign.fill.initials.remove",
    "sign.fill.initials.change",
    "optimize.reduce",
    "optimize.advanced",
    "ocr.recognize",
    "a11y.check",
    "a11y.find_untagged",
    "a11y.autotag",
    "a11y.report",
    "a11y.reading_options",
    "a11y.alt_text",
    "a11y.autotag_fields",
    "a11y.tags",
    "edit.header_footer",
    "edit.header_footer.update",
    "edit.header_footer.remove",
    "edit.watermark",
    "edit.watermark.update",
    "edit.watermark.remove",
    "edit.background",
    "edit.background.update",
    "edit.background.remove",
    "protect.password",
    "protect.remove",
    "protect.unlock",
    "protect.properties",
    "page.organize",
    "bookmark.add",
    "bookmark.from_structure",
    "page.rotate",
    "page.rotate_ccw",
    "page.delete",
    "page.insert_blank",
    "page.rotate_dialog",
    "page.duplicate",
    "page.reverse",
    "page.crop",
    "page.boxes",
    "page.extract",
    "page.number",
];

/// Commands no host build runs: they open the user's files, save or export to them, print,
/// reach the web, add local files to a document, keep it in the browser, or change the settings
/// that turn those back on.
// Read by the drift test only: what is in no allow list is denied anyway.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const DENIED: &[&str] = &[
    "file.open",
    "file.open_recent",
    "file.clear_recent",
    "file.pin_folder",
    "file.save_as",
    "file.close",
    "file.close_all",
    "file.revert",
    "create.blank",
    "create.file",
    "create.multiple",
    "create.multiple_separate",
    "create.images",
    "create.clipboard",
    "measure.export",
    "print.dialog",
    "comment.export",
    "form.export_data",
    "actions.wizard",
    "actions.distribution",
    "actions.optimize_scans",
    "export.image",
    "export.text",
    "export.docx",
    "export.pptx",
    "export.html",
    "export.rtf",
    "export.all_images",
    "app.preferences",
    "tools.js_console",
    "tools.document_js",
    "ocr.enhance_camera",
    "ocr.recognize_batch",
    "page.split",
    "help.discord",
    "help.app_page",
    "help.github",
    "help.website",
    "help.check_updates",
];

/// The commands each policy group turns on. The first three groups' commands are in [`DENIED`]
/// and are the only way out of it; the others' are in [`BUILDER`], which a viewer runs when its
/// policy names the group. A command is in at most one group.
pub(crate) const GROUPS: &[(PolicyGroup, &[&str])] = &[
    (PolicyGroup::Print, &["print.dialog"]),
    (
        PolicyGroup::Export,
        &[
            "file.save_as",
            "measure.export",
            "comment.export",
            "form.export_data",
            "export.image",
            "export.text",
            "export.docx",
            "export.pptx",
            "export.html",
            "export.rtf",
            "export.all_images",
            "page.split",
        ],
    ),
    (PolicyGroup::ExternalLinks, &["help.discord", "help.app_page", "help.github", "help.website", "help.check_updates"]),
    (PolicyGroup::Measure, &["measure.distance", "measure.perimeter", "measure.area", "measure.scale", "measure.info", "measure.snap"]),
    (
        PolicyGroup::FillSign,
        &[
            "sign.fill.text",
            "sign.fill.check",
            "sign.fill.cross",
            "sign.fill.dot",
            "sign.fill.line",
            "sign.fill.date",
            "sign.fill.signature",
            "sign.fill.initials",
            "sign.fill.signature.change",
            "sign.fill.signature.remove",
            "sign.fill.initials.remove",
            "sign.fill.initials.change",
        ],
    ),
    (
        PolicyGroup::PageEdit,
        &[
            "page.copy",
            "page.cut",
            "page.paste",
            "page.organize",
            "page.rotate",
            "page.rotate_ccw",
            "page.delete",
            "page.insert_blank",
            "page.rotate_dialog",
            "page.duplicate",
            "page.reverse",
            "page.crop",
            "page.boxes",
            "page.extract",
            "page.number",
            "page.combine",
            "page.insert",
            "page.replace",
        ],
    ),
    (PolicyGroup::ContentEdit, &["edit.text", "edit.edit_text", "edit.link", "edit.links_from_urls", "edit.remove_links"]),
];

/// The group that turns `id` on, if there is one.
pub(crate) fn group_of(id: &str) -> Option<PolicyGroup> {
    GROUPS.iter().find(|(_, ids)| ids.contains(&id)).map(|(g, _)| *g)
}

/// Groups that can lift the lock on [`DENIED`] commands; the others only widen a viewer.
#[cfg_attr(not(test), allow(dead_code))]
fn opens_denied(group: PolicyGroup) -> bool {
    matches!(group, PolicyGroup::Print | PolicyGroup::Export | PolicyGroup::ExternalLinks)
}

/// Whether a downloaded file may leave (the host's policy turned exporting or printing on for
/// the document being shown). Platform code that cannot reach the app reads it.
static DOWNLOADS_OPEN: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_downloads_open(open: bool) {
    DOWNLOADS_OPEN.store(open, Ordering::Relaxed);
}

// Read by the web build's download only.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn downloads_open() -> bool {
    DOWNLOADS_OPEN.load(Ordering::Relaxed)
}

/// Whether a document with `own` may have work of `group` done on it: a builder's always, a
/// viewer's when its policy names the group (`None`: never, nothing opens that to a viewer).
pub(crate) fn scope_allows(own: &Ownership, group: Option<PolicyGroup>) -> bool {
    own.scope == EditScope::All || group.is_some_and(|g| own.allow.has(g))
}

/// Who may run a command in a host build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Allow {
    /// Viewer and builder.
    Viewer,
    /// Builder only.
    Builder,
}

/// What a host build lets `id` do; `None` is denied, and so is any command not in the lists.
pub(crate) fn allowed(id: &str) -> Option<Allow> {
    if VIEWER.contains(&id) {
        Some(Allow::Viewer)
    } else if BUILDER.contains(&id) {
        Some(Allow::Builder)
    } else {
        None
    }
}

/// The groups of `pdfcraft:open` `policy.allow`: `None` when there is no policy or its `allow`
/// is not a list, and a list with anything but text in it counts as no list. Ids that are not a
/// group are ignored.
pub fn policy_groups(allow: Option<&[Option<String>]>) -> pdfcraft_engine::PolicyGroups {
    match allow {
        Some(ids) if ids.iter().all(Option::is_some) => pdfcraft_engine::PolicyGroups::from_ids(ids.iter().flatten().map(String::as_str)),
        _ => Default::default(),
    }
}

/// The rules of a document the host opened without naming any: a viewer's, the narrowest.
pub(crate) fn viewer_ownership() -> pdfcraft_engine::Ownership {
    pdfcraft_engine::Ownership { scope: EditScope::Viewer, ..Default::default() }
}

/// The page every host build shows, and nothing else, when no embedding page is connected.
pub(crate) fn open_in_ppm_screen(ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.centered_and_justified(|ui| {
            ui.label(egui::RichText::new("PPM에서 열어 주세요").size(22.0));
        });
    });
}

use crate::PdfCraftApp;

impl PdfCraftApp {
    /// This build is made for an embedding page (see [`HOST_LOCK`]).
    pub fn host_locked(&self) -> bool {
        self.host_lock
    }

    /// The scope of the active document; a viewer's when there is none.
    fn host_scope(&self) -> EditScope {
        self.active_ids().and_then(|(_, id)| self.session.get(id)).map_or(EditScope::Viewer, |d| d.ownership().scope)
    }

    /// What the host's policy turned on for the active document; nothing with no document.
    fn host_groups(&self) -> pdfcraft_engine::PolicyGroups {
        self.active_ids().and_then(|(_, id)| self.session.get(id)).map_or_else(Default::default, |d| d.ownership().allow)
    }

    /// Whether the host's policy turned `group` on for the active document.
    pub(crate) fn host_group(&self, group: PolicyGroup) -> bool {
        self.host_groups().has(group)
    }

    /// A locked build showing a viewer's document (or none): the interface offers comments and
    /// looking around, and hides the tools that edit the document itself. A policy that opens
    /// pages, content or measuring to the viewer brings the tool panels back.
    pub(crate) fn host_viewer(&self) -> bool {
        self.host_lock
            && self.host_scope() == EditScope::Viewer
            && ![PolicyGroup::PageEdit, PolicyGroup::ContentEdit, PolicyGroup::Measure].into_iter().any(|g| self.host_group(g))
    }

    /// Whether the host lock lets command `id` run now. Always true without the lock.
    pub(crate) fn host_allows(&self, id: &str) -> bool {
        if !self.host_lock {
            return true;
        }
        let by_policy = || group_of(id).is_some_and(|g| self.host_group(g));
        match allowed(id) {
            Some(Allow::Viewer) => true,
            Some(Allow::Builder) => self.host_scope() == EditScope::All || by_policy(),
            None => by_policy(),
        }
    }

    /// A locked build with no embedding page connected: it opens nothing and shows only
    /// [`open_in_ppm_screen`]. Fails closed, so a page opened on its own, without `?host=parent`
    /// or a same-origin parent, never gets a working app.
    pub(crate) fn host_missing(&self) -> bool {
        self.host_lock && self.host_save.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LinkOrigin, pickers::PickFor};
    use pdfcraft_engine::commands::COMMANDS;
    use pdfcraft_engine::Ownership;

    /// One blank page.
    fn pdf() -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 200 300] >>",
            "<< /Type /Page /Parent 2 0 R >>",
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

    fn locked() -> PdfCraftApp {
        let mut app = PdfCraftApp::new();
        app.host_lock = true;
        app
    }

    /// Every command is classified once: a new one is in no list and fails here until someone
    /// decides what a host build does with it (until then it is denied).
    #[test]
    fn every_command_is_classified_exactly_once() {
        let mut seen = std::collections::HashMap::new();
        for (list, ids) in [("VIEWER", VIEWER), ("BUILDER", BUILDER), ("DENIED", DENIED)] {
            for id in ids {
                assert!(COMMANDS.iter().any(|c| c.id == *id), "{id} in {list} is not a command (renamed or removed?)");
                if let Some(other) = seen.insert(*id, list) {
                    panic!("{id} is in both {other} and {list}");
                }
            }
        }
        let missing: Vec<&str> = COMMANDS.iter().map(|c| c.id).filter(|id| !seen.contains_key(id)).collect();
        assert!(missing.is_empty(), "commands with no host classification (add them to VIEWER, BUILDER or DENIED in host_lock.rs): {missing:?}");
    }

    /// What the contract names as shut in every host build is in no allow list.
    #[test]
    fn what_reaches_the_users_files_or_the_web_is_denied() {
        for id in [
            "file.open", "file.open_recent", "file.save_as", "create.blank", "create.file", "print.dialog", "export.image", "export.docx",
            "comment.export", "tools.js_console", "tools.document_js",
            "app.preferences", "help.discord", "help.github", "help.website", "help.check_updates", "help.app_page",
        ] {
            assert_eq!(allowed(id), None, "{id}");
        }
        assert_eq!(allowed("help.about"), Some(Allow::Viewer));
        assert_eq!(allowed("file.save"), Some(Allow::Viewer));
        assert_eq!(allowed("not.a.command"), None, "what is unknown is denied");
    }

    /// Every command a group turns on is in the list that group widens, in one group only, and
    /// what no policy may turn on is in none.
    #[test]
    fn every_policy_group_names_its_commands_once() {
        let mut seen = std::collections::HashMap::new();
        for (group, ids) in GROUPS {
            assert!(!ids.is_empty(), "{group:?}");
            for id in *ids {
                assert!(COMMANDS.iter().any(|c| c.id == *id), "{id} in {group:?} is not a command");
                if let Some(other) = seen.insert(*id, *group) {
                    panic!("{id} is in both {other:?} and {group:?}");
                }
                let list = if opens_denied(*group) { DENIED } else { BUILDER };
                assert!(list.contains(id), "{id} ({group:?}) should be in {}", if opens_denied(*group) { "DENIED" } else { "BUILDER" });
            }
        }
        for g in PolicyGroup::ALL {
            assert!(GROUPS.iter().any(|(x, _)| *x == g), "{g:?} turns nothing on");
        }
        for id in [
            "file.open", "file.open_recent", "file.clear_recent", "file.close", "file.revert", "create.blank", "create.file", "create.images",
            "tools.js_console", "tools.document_js", "app.preferences",
        ] {
            assert_eq!(group_of(id), None, "{id} can't be turned on by a policy");
        }
    }

    #[test]
    fn a_policy_that_is_missing_or_malformed_turns_nothing_on() {
        let s = |v: &[&str]| v.iter().map(|x| Some(x.to_string())).collect::<Vec<_>>();
        assert_eq!(policy_groups(None), Default::default());
        assert_eq!(policy_groups(Some(&[Some("print".into()), None])), Default::default(), "one item that isn't text spoils the list");
        assert_eq!(policy_groups(Some(&s(&["nope", "insert_local"]))), Default::default(), "unknown ids are ignored");
        let on = policy_groups(Some(&s(&["print", "page_edit", "nope"])));
        assert!(on.has(PolicyGroup::Print) && on.has(PolicyGroup::PageEdit) && !on.has(PolicyGroup::Export));
    }

    /// A viewer gets what its policy names, and nothing else.
    #[test]
    fn a_policy_widens_a_viewer_by_group_only() {
        let mut app = locked();
        let allow = |ids: &[&str]| Ownership { scope: EditScope::Viewer, allow: policy_groups(Some(&ids.iter().map(|x| Some(x.to_string())).collect::<Vec<_>>())), ..Default::default() };
        app.session.set_open_ownership(allow(&["print", "page_edit", "fill_sign"]));
        app.open_bytes("p.pdf", None, pdf()).unwrap();
        assert!(app.host_allows("print.dialog") && app.host_allows("page.rotate") && app.host_allows("sign.fill.text"));
        assert!(!app.host_allows("file.save_as") && !app.host_allows("measure.distance") && !app.host_allows("edit.text"));
        assert!(!app.host_allows("file.open") && !app.host_allows("redact.apply") && !app.host_allows("help.github"));
        assert!(!app.host_viewer(), "page tools come back with page_edit");
        assert!(app.host_group(PolicyGroup::Print) && !app.host_group(PolicyGroup::Export));
        // The engine follows the same policy.
        let (_, id) = app.active_ids().unwrap();
        assert!(app.session.apply(id, pdfcraft_engine::Edit::RotatePages { pages: vec![0], degrees: 90 }).is_ok());
        app.session.set_open_ownership(allow(&["fill_sign"]));
        app.open_bytes("q.pdf", None, pdf()).unwrap();
        assert!(app.host_viewer() && app.host_allows("sign.fill.text") && !app.host_allows("page.rotate"));
    }

    /// `pdfcraft:open` `mode`: `builder` is the only way to the wider scope.
    #[test]
    fn the_mode_value_is_passed_through_and_defaults_to_the_narrowest() {
        assert_eq!(parse_mode(Some("builder")), EditScope::All);
        assert_eq!(parse_mode(Some("viewer")), EditScope::Viewer);
        assert_eq!(parse_mode(None), EditScope::Viewer, "no mode, or a value of another type");
        for odd in ["", "Builder", "admin", "builder ", "all"] {
            assert_eq!(parse_mode(Some(odd)), EditScope::Viewer, "{odd:?}");
        }
        // Without the lock there are no modes.
        if !HOST_LOCK {
            assert_eq!(open_scope(Some("viewer")), EditScope::All);
        } else {
            assert_eq!(open_scope(Some("viewer")), EditScope::Viewer);
            assert_eq!(open_scope(Some("builder")), EditScope::All);
        }
    }

    #[test]
    fn a_locked_build_runs_only_what_the_scope_allows() {
        let mut app = locked();
        assert!(app.host_allows("comment.note") && app.host_allows("help.about"));
        assert!(!app.host_allows("file.open") && !app.host_allows("page.rotate"), "no document: a viewer's commands only");
        app.session.set_open_ownership(Ownership { scope: EditScope::Viewer, ..Default::default() });
        app.open_bytes("v.pdf", None, pdf()).unwrap();
        assert!(!app.host_allows("page.rotate") && !app.host_allows("redact.apply"));
        assert!(app.host_viewer() && !app.host_allows("sign.fill.text") && !app.host_allows("sign.fill.signature"), "Fill & Sign is not in /viewer");
        assert!(!app.execute("page.rotate") && !app.execute("file.open") && !app.execute("print.dialog"), "refused without a word");
        assert!(app.toast.is_none() && app.dialog.is_none());
        app.session.set_open_ownership(Ownership { scope: EditScope::All, ..Default::default() });
        app.open_bytes("b.pdf", None, pdf()).unwrap();
        assert!(app.host_allows("page.rotate") && app.host_allows("redact.apply"), "a builder edits the document");
        assert!(!app.host_viewer() && app.host_allows("sign.fill.text"), "and fills and signs");
        assert!(!app.host_allows("file.open") && !app.host_allows("file.save_as") && !app.host_allows("print.dialog"), "but still not the files");
        // The same commands run in a build without the lock.
        app.host_lock = false;
        assert!(app.host_allows("file.open"));
    }

    #[test]
    fn a_locked_build_shows_no_blocked_command_in_the_palette_or_menus() {
        let app = locked();
        let shown: Vec<&str> = COMMANDS.iter().map(|c| c.id).filter(|id| app.host_allows(id)).collect();
        assert!(shown.contains(&"comment.note") && shown.contains(&"help.about"));
        for id in DENIED.iter().chain(BUILDER) {
            assert!(!shown.contains(id), "{id}");
        }
    }

    #[test]
    fn a_locked_build_opens_nothing_the_user_picks_or_pastes() {
        let mut app = locked();
        app.pick_override = Some(vec!["/does/not/matter.pdf".into()]);
        app.open_dialog();
        app.pick(PickFor::Open, rfd::AsyncFileDialog::new(), true);
        assert!(!app.pickers.pending(), "no picker was started");
        app.open_recent("/does/not/matter.pdf");
        app.open_path("/does/not/matter.pdf");
        assert!(app.views.is_empty() && app.toast.is_none());
        // What lands in the user's inbox (a pick, a drop) is never opened.
        app.inbox.lock().unwrap().push(("x.pdf".into(), pdf()));
        let mut h = egui_kittest::Harness::builder().build_eframe(move |_| app);
        h.run_steps(3);
        assert!(h.state().views.is_empty(), "the user's file was not opened");
    }

    #[test]
    fn a_locked_build_opens_no_web_links() {
        let mut app = locked();
        app.open_url("https://example.com");
        assert_eq!(app.last_opened_url, None);
        app.request_document_url("https://example.com", LinkOrigin::Link);
        assert!(app.pending_link.is_none(), "a document's link doesn't even ask");
        app.host_lock = false;
        app.open_url("https://example.com");
        assert_eq!(app.last_opened_url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn a_locked_build_without_a_host_opens_no_document() {
        let app = locked();
        assert!(app.host_missing());
        app.startup_inbox.lock().unwrap().push(("doc.pdf".into(), pdf()));
        let mut h = egui_kittest::Harness::builder().build_eframe(move |_| app);
        h.run_steps(4);
        assert!(h.state().views.is_empty(), "no host: nothing opens");
        assert_eq!(h.state().startup_inbox.lock().unwrap().len(), 1, "the file stays where it was, unread");
        // With a host connected the same file opens, as a viewer's.
        h.state_mut().host_save = Some(Box::new(|_, _, _| Ok(())));
        h.run_steps(4);
        let state = h.state();
        assert_eq!(state.views.len(), 1);
        let (_, id) = state.active_ids().unwrap();
        assert_eq!(state.session.get(id).unwrap().ownership().scope, EditScope::Viewer, "no mode sent: the narrowest");
    }

    #[test]
    fn the_hosts_mode_reaches_the_document() {
        let app = locked();
        app.startup_inbox.lock().unwrap().push(("doc.pdf".into(), pdf()));
        app.ownership_inbox.lock().unwrap().push(Ownership { scope: open_scope_for_test("builder"), ..Default::default() });
        let mut app = app;
        app.host_save = Some(Box::new(|_, _, _| Ok(())));
        let mut h = egui_kittest::Harness::builder().build_eframe(move |_| app);
        h.run_steps(4);
        let state = h.state();
        let (_, id) = state.active_ids().unwrap();
        assert_eq!(state.session.get(id).unwrap().ownership().scope, EditScope::All);
    }

    /// `parse_mode`, whatever the build: the document gets what the host said.
    fn open_scope_for_test(mode: &str) -> EditScope {
        parse_mode(Some(mode))
    }
}
