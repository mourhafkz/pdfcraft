//! The Library tab: a reference manager in the spirit of Mendeley.
//!
//! Left: the library's sections, collections and tags. Middle: the papers the selection and
//! search admit, as a sortable table. Right: the selected paper's details in three tabs —
//! **Info** (editable metadata, tags, collections, DOI/arXiv lookup), **Notes** (free-form
//! Markdown kept with the paper) and **Highlights** (every highlight and comment in the PDF,
//! coloured as drawn, with the text under it and the note written on it; a click opens the page).
//!
//! The library lives in `<settings dir>/library/` (see [`pdfcraft_library::Library`]). Reading a
//! PDF (identifiers, full text, highlights) and metadata lookups run on worker threads; their
//! answers arrive on a channel and are applied on a later frame.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::{Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, vec2};
use pdfcraft_library::cite;
use pdfcraft_library::model::{Author, Metadata, ReadState};
use pdfcraft_library::search::Scope;
use pdfcraft_library::{CollectionId, Filter, Library, PaperId, SortKey};

use crate::library_marks::{self, Mark};
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

/// Changes are written this long after the last edit (and when the app saves its state).
const SAVE_DELAY: Duration = Duration::from_millis(1200);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Info,
    Notes,
    Highlights,
}

/// Work done off the interface thread.
enum Job {
    Imported { id: PaperId, result: Result<Box<library_marks::Scanned>, String>, lookup: Option<Result<Metadata, String>> },
    Looked { id: PaperId, result: Result<Metadata, String>, overwrite: bool },
    Marks { id: PaperId, result: Result<Vec<Mark>, String> },
    Hits { generation: u64, hits: Vec<(PaperId, String)> },
}

/// Text fields that don't map one-to-one onto the record, held while the user types.
#[derive(Default)]
struct Drafts {
    paper: Option<PaperId>,
    authors: String,
    year: String,
    keywords: String,
    new_tag: String,
}

/// The Library tab's state.
pub struct LibraryUi {
    /// The tab is in the strip.
    pub open: bool,
    /// The tab is the one showing (with no document tab active).
    pub focused: bool,
    lib: Option<Library>,
    load_error: Option<String>,
    filter: Filter,
    sort: SortKey,
    ascending: bool,
    selected: Vec<PaperId>,
    detail: DetailTab,
    drafts: Drafts,
    tx: Sender<Job>,
    rx: Receiver<Job>,
    /// Papers being read or looked up.
    busy: HashSet<PaperId>,
    marks: HashMap<PaperId, Vec<Mark>>,
    marks_loading: HashSet<PaperId>,
    /// Show only highlights of this colour.
    color_filter: Option<String>,
    marks_by_color: bool,
    fulltext_query: String,
    fulltext_generation: u64,
    fulltext_hits: Vec<(PaperId, String)>,
    query_changed: Option<Instant>,
    new_collection: Option<String>,
    renaming: Option<(CollectionId, String)>,
    last_edit: Option<Instant>,
    /// Open this file and go to this page once its tab exists.
    pending_jump: Option<(String, usize)>,
    /// The floating notes window beside a document that is in the library.
    pub notes_window: bool,
    watch_scanned: bool,
}

impl Default for LibraryUi {
    fn default() -> Self {
        let (tx, rx) = channel();
        LibraryUi {
            open: false,
            focused: false,
            lib: None,
            load_error: None,
            filter: Filter::default(),
            sort: SortKey::Added,
            ascending: false,
            selected: Vec::new(),
            detail: DetailTab::Info,
            drafts: Drafts::default(),
            tx,
            rx,
            busy: HashSet::new(),
            marks: HashMap::new(),
            marks_loading: HashSet::new(),
            color_filter: None,
            marks_by_color: true,
            fulltext_query: String::new(),
            fulltext_generation: 0,
            fulltext_hits: Vec::new(),
            query_changed: None,
            new_collection: None,
            renaming: None,
            last_edit: None,
            pending_jump: None,
            notes_window: false,
            watch_scanned: false,
        }
    }
}

/// Where the library is kept: `PDFCRAFT_LIBRARY_DIR`, else the portable data folder, else the
/// app's settings folder.
fn library_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("PDFCRAFT_LIBRARY_DIR") {
        return PathBuf::from(d);
    }
    crate::portable::data_dir()
        .map(std::path::Path::to_path_buf)
        .or_else(|| eframe::storage_dir("PdfCraft"))
        .unwrap_or_else(|| std::env::temp_dir().join("PdfCraft"))
        .join("library")
}

fn spawn(f: impl FnOnce() + Send + 'static) {
    if std::thread::Builder::new().name("library".into()).spawn(f).is_err() {
        log::warn!("library: couldn't start a worker thread");
    }
}

impl LibraryUi {
    fn ensure_loaded(&mut self) {
        if self.lib.is_none() {
            let (lib, err) = Library::open(library_dir());
            if let Some(e) = err {
                log::warn!("library: {e}");
                self.load_error = Some(e.to_string());
            }
            self.lib = Some(lib);
        }
    }

    /// The paper record for a file, if it's in the library (loads the library on first use).
    pub fn paper_for(&mut self, path: &str) -> Option<PaperId> {
        self.ensure_loaded();
        self.lib.as_ref()?.find_path(path)
    }

    fn edited(&mut self, id: PaperId) {
        if let Some(lib) = self.lib.as_mut() {
            lib.touch(id);
        }
        self.last_edit = Some(Instant::now());
    }

    /// Whether papers are still being read, looked up or searched (screenshots wait for it).
    pub fn working(&self) -> bool {
        !self.busy.is_empty() || !self.marks_loading.is_empty() || self.query_changed.is_some()
    }

    /// Write pending changes (also called when the app saves its state and on exit).
    pub fn save_now(&mut self) {
        if let Some(lib) = self.lib.as_mut()
            && let Err(e) = lib.save()
        {
            log::warn!("library: {e}");
            self.load_error = Some(e.to_string());
        }
        self.last_edit = None;
    }
}

impl PdfCraftApp {
    /// Show the Library tab.
    pub fn open_library_tab(&mut self) {
        self.library.open = true;
        self.library.focused = true;
        self.library.ensure_loaded();
        self.combine_tab.focused = false;
        self.active = None;
    }

    /// Select a paper by id and show its details (the `library` option, screenshots, tests).
    pub fn library_select(&mut self, id: u64) {
        self.library.selected = vec![PaperId(id)];
    }

    /// Type into the library's search box (screenshots, tests).
    pub fn library_search(&mut self, query: &str) {
        query.clone_into(&mut self.library.filter.query);
        self.library.query_changed = Some(Instant::now() - Duration::from_secs(1));
    }

    /// Show this details tab: `info`, `notes` or `highlights`.
    pub fn library_detail(&mut self, tab: &str) {
        self.library.detail = match tab {
            "notes" => DetailTab::Notes,
            "highlights" => DetailTab::Highlights,
            _ => DetailTab::Info,
        };
    }

    /// Whether the Library tab is the one showing.
    pub fn library_showing(&self) -> bool {
        self.active.is_none() && self.library.open && self.library.focused
    }

    /// Add PDFs to the library and read them in the background.
    pub fn library_add_paths(&mut self, paths: Vec<PathBuf>) {
        self.library.ensure_loaded();
        let mut added = 0usize;
        let mut last = None;
        for p in paths {
            if p.is_dir() {
                let mut found = Vec::new();
                collect_pdfs(&p, 0, &mut found);
                for f in found {
                    added += usize::from(self.library_add_one(&f.to_string_lossy()).is_some_and(|(_, new)| new));
                }
                continue;
            }
            if let Some((id, new)) = self.library_add_one(&p.to_string_lossy()) {
                added += usize::from(new);
                last = Some(id);
            }
        }
        if let Some(id) = last {
            self.library.selected = vec![id];
        }
        if added > 0 {
            self.library.last_edit = Some(Instant::now());
            self.notify(format!("Added {added} paper{} to the library", if added == 1 { "" } else { "s" }));
        }
    }

    fn library_add_one(&mut self, path: &str) -> Option<(PaperId, bool)> {
        let lib = self.library.lib.as_mut()?;
        let (id, new) = lib.add(path);
        if new {
            self.library_import(id);
        }
        Some((id, new))
    }

    /// Read a paper: identifiers and title, full text, highlights; then look it up if allowed.
    fn library_import(&mut self, id: PaperId) {
        let Some(lib) = self.library.lib.as_ref() else { return };
        let Some(paper) = lib.get(id) else { return };
        let path = paper.path.clone();
        let auto_lookup = lib.settings.auto_lookup;
        let dir = lib.dir().to_path_buf();
        let tx = self.library.tx.clone();
        let ctx = self.ctx.clone();
        self.library.busy.insert(id);
        spawn(move || {
            let result = library_marks::read_file(&path).and_then(|b| library_marks::scan(b, true));
            if let Ok(s) = &result
                && let Err(e) = Library::store_fulltext(&dir, id, &s.text)
            {
                log::warn!("library: couldn't keep the text of {path}: {e}");
            }
            let lookup = match &result {
                Ok(s) if auto_lookup && (s.found.doi.is_some() || s.found.arxiv.is_some() || s.found.title.is_some()) => {
                    let probe = Metadata {
                        doi: s.found.doi.clone().unwrap_or_default(),
                        arxiv: s.found.arxiv.clone().unwrap_or_default(),
                        title: s.found.title.clone().unwrap_or_default(),
                        authors: s.found.authors.clone(),
                        ..Default::default()
                    };
                    Some(lookup(&probe))
                }
                _ => None,
            };
            let result = result.map(|mut s| {
                s.text = String::new();
                Box::new(s)
            });
            let _ = tx.send(Job::Imported { id, result, lookup });
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn library_lookup(&mut self, id: PaperId, overwrite: bool) {
        let Some(paper) = self.library.lib.as_ref().and_then(|l| l.get(id)) else { return };
        let meta = paper.meta.clone();
        let tx = self.library.tx.clone();
        let ctx = self.ctx.clone();
        self.library.busy.insert(id);
        spawn(move || {
            let _ = tx.send(Job::Looked { id, result: lookup(&meta), overwrite });
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    /// Read a paper's highlights: from the open tab when it's open (unsaved highlights count),
    /// else from its file.
    fn library_load_marks(&mut self, id: PaperId) {
        let Some(path) = self.library.lib.as_ref().and_then(|l| l.get(id)).map(|p| p.path.clone()) else { return };
        let open_bytes =
            self.tab_showing(&path).and_then(|i| self.views.get(i)).and_then(|v| self.session.get(v.id)).map(|d| d.export_source().bytes);
        let tx = self.library.tx.clone();
        let ctx = self.ctx.clone();
        self.library.marks_loading.insert(id);
        spawn(move || {
            let bytes = match open_bytes {
                Some(b) => Ok(b),
                None => library_marks::read_file(&path),
            };
            let result = bytes.and_then(|b| library_marks::scan(b, false)).map(|s| s.marks);
            let _ = tx.send(Job::Marks { id, result });
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn library_search_fulltext(&mut self) {
        let Some(lib) = self.library.lib.as_ref() else { return };
        self.library.fulltext_generation = self.library.fulltext_generation.wrapping_add(1);
        let generation = self.library.fulltext_generation;
        let query = self.library.filter.query.clone();
        self.library.fulltext_query.clone_from(&query);
        if query.trim().is_empty() {
            self.library.fulltext_hits.clear();
            return;
        }
        let dir = lib.dir().to_path_buf();
        let ids: Vec<PaperId> = lib.papers().map(|p| p.id).collect();
        let tx = self.library.tx.clone();
        let ctx = self.ctx.clone();
        spawn(move || {
            let hits = Library::fulltext_hits(&dir, &ids, &query);
            let _ = tx.send(Job::Hits { generation, hits });
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    /// Apply finished work, save after edits settle, jump to a highlight's page once its tab
    /// opened. Runs every frame, whichever tab shows.
    pub fn library_frame(&mut self) {
        while let Ok(job) = self.library.rx.try_recv() {
            self.library_apply(job);
        }
        if self.library.last_edit.is_some_and(|t| t.elapsed() >= SAVE_DELAY) {
            self.library.save_now();
        } else if let (Some(t), Some(ctx)) = (self.library.last_edit, self.ctx.as_ref()) {
            ctx.request_repaint_after(SAVE_DELAY.saturating_sub(t.elapsed()));
        }
        if self.library.query_changed.is_some_and(|t| t.elapsed() >= Duration::from_millis(300)) {
            self.library.query_changed = None;
            self.library_search_fulltext();
        }
        if let Some((path, page)) = self.library.pending_jump.clone()
            && let Some(i) = self.tab_showing(&path)
        {
            self.library.pending_jump = None;
            if let Some(view) = self.views.get_mut(i) {
                view.go_to_page(page);
            }
            self.active = Some(i);
            self.library.focused = false;
        }
    }

    fn library_apply(&mut self, job: Job) {
        let Some(lib) = self.library.lib.as_mut() else { return };
        match job {
            Job::Imported { id, result, lookup } => {
                self.library.busy.remove(&id);
                let Some(paper) = lib.get_mut(id) else { return };
                match result {
                    Ok(s) => {
                        let mut found = Metadata {
                            title: s.found.title.clone().unwrap_or_default(),
                            doi: s.found.doi.clone().unwrap_or_default(),
                            arxiv: s.found.arxiv.clone().unwrap_or_default(),
                            isbn: s.found.isbn.clone().unwrap_or_default(),
                            venue: s.found.header.venue.clone().unwrap_or_default(),
                            year: s.found.header.year,
                            volume: s.found.header.volume.clone().unwrap_or_default(),
                            issue: s.found.header.issue.clone().unwrap_or_default(),
                            ..Default::default()
                        };
                        if let Some(a) = s.info_author.as_deref().filter(|a| !a.trim().is_empty()) {
                            found.authors = Author::parse_list(a);
                        }
                        if found.authors.is_empty() {
                            found.authors = s.found.authors.clone();
                        }
                        if let Some(k) = s.info_keywords.as_deref() {
                            found.keywords = k.split([',', ';']).map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned).collect();
                        }
                        // A lookup's answer is better than guesses from the file.
                        match lookup {
                            Some(Ok(m)) => {
                                paper.meta.fill_from(&m);
                                paper.lookup_tried = true;
                            }
                            Some(Err(e)) => {
                                paper.lookup_tried = true;
                                log::info!("library: lookup for {} failed: {e}", paper.path);
                            }
                            None => {}
                        }
                        paper.meta.fill_from(&found);
                        if paper.meta.kind.is_empty() {
                            paper.meta.kind = "article".into();
                        }
                        self.library.marks.insert(id, s.marks);
                    }
                    Err(e) => log::warn!("library: couldn't read {}: {e}", paper.path),
                }
                lib.touch(id);
                self.library.drafts.paper = None;
                self.library.last_edit = Some(Instant::now());
            }
            Job::Looked { id, result, overwrite } => {
                self.library.busy.remove(&id);
                match result {
                    Ok(m) => {
                        if let Some(p) = lib.get_mut(id) {
                            if overwrite {
                                p.meta.overwrite_from(&m);
                            } else {
                                p.meta.fill_from(&m);
                            }
                            p.lookup_tried = true;
                        }
                        lib.touch(id);
                        self.library.drafts.paper = None;
                        self.library.last_edit = Some(Instant::now());
                        self.notify("Details updated from the online record");
                    }
                    Err(e) => self.notify(format!("Lookup failed: {e}")),
                }
            }
            Job::Marks { id, result } => {
                self.library.marks_loading.remove(&id);
                match result {
                    Ok(m) => {
                        self.library.marks.insert(id, m);
                    }
                    Err(e) => {
                        log::warn!("library: highlights: {e}");
                        self.library.marks.insert(id, Vec::new());
                    }
                }
            }
            Job::Hits { generation, hits } => {
                if generation == self.library.fulltext_generation {
                    self.library.fulltext_hits = hits;
                }
            }
        }
    }

    /// Open a paper in a tab (at `page`, when given) and remember when it was opened.
    fn library_open_paper(&mut self, id: PaperId, page: Option<usize>) {
        let Some(lib) = self.library.lib.as_mut() else { return };
        let Some(paper) = lib.get_mut(id) else { return };
        let path = paper.path.clone();
        if !std::path::Path::new(&path).is_file() {
            self.notify("The PDF isn't where it was (moved or deleted). Remove it, or add it again from its new place.");
            return;
        }
        paper.last_opened = pdfcraft_library::store::now();
        if paper.read_state == ReadState::Unread {
            paper.read_state = ReadState::Reading;
        }
        lib.touch(id);
        self.library.last_edit = Some(Instant::now());
        self.library.pending_jump = Some((path.clone(), page.unwrap_or(0)));
        let _ = self.open_path(&path);
        // A file already open comes to the front at once.
        self.library_frame();
    }

    fn library_scan_watched(&mut self) {
        let Some(lib) = self.library.lib.as_ref() else { return };
        let new: Vec<PathBuf> = lib.scan_watched().into_iter().map(PathBuf::from).collect();
        if !new.is_empty() {
            self.library_add_paths(new);
        }
    }
}

#[cfg(feature = "library-fetch")]
fn lookup(meta: &Metadata) -> Result<Metadata, String> {
    pdfcraft_library::fetch::lookup(meta).map_err(|e| e.to_string())
}

#[cfg(not(feature = "library-fetch"))]
fn lookup(_meta: &Metadata) -> Result<Metadata, String> {
    Err("this build has no online lookup".into())
}

fn collect_pdfs(dir: &std::path::Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 6 || out.len() > 20_000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if p.is_dir() {
            collect_pdfs(&p, depth + 1, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")) {
            out.push(p);
        }
    }
}

// ---- drawing -------------------------------------------------------------------------------------

pub fn page(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    app.library.ensure_loaded();
    if !app.library.watch_scanned {
        app.library.watch_scanned = true;
        app.library_scan_watched();
    }
    let t = Tokens::get(ui.ctx());
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, t.panel);
    let side_w = 230.0f32.min(full.width() * 0.22);
    let detail_w = 380.0f32.min(full.width() * 0.34);
    let side = egui::Rect::from_min_size(full.min, vec2(side_w, full.height()));
    let detail = egui::Rect::from_min_max(egui::pos2(full.max.x - detail_w, full.min.y), full.max);
    let middle = egui::Rect::from_min_max(egui::pos2(side.max.x, full.min.y), egui::pos2(detail.min.x, full.max.y));
    ui.painter().rect_filled(side, 0.0, t.chrome);
    ui.painter().vline(side.max.x, full.y_range(), Stroke::new(1.0, t.divider));
    ui.painter().vline(detail.min.x, full.y_range(), Stroke::new(1.0, t.divider));
    ui.painter().rect_filled(middle, 0.0, t.card);

    ui.scope_builder(egui::UiBuilder::new().max_rect(side.shrink2(vec2(10.0, 12.0))), |ui| sidebar(app, ui, &t));
    ui.scope_builder(egui::UiBuilder::new().max_rect(middle.shrink2(vec2(14.0, 12.0))), |ui| {
        ui.set_clip_rect(middle);
        paper_list(app, ui, &t);
    });
    ui.scope_builder(egui::UiBuilder::new().max_rect(detail.shrink2(vec2(14.0, 12.0))), |ui| {
        ui.set_clip_rect(detail);
        details(app, ui, &t);
    });
}

fn section_label(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(text.to_uppercase()).font(theme::semibold(10.5)).color(t.text_faint));
    ui.add_space(2.0);
}

/// One sidebar row; returns whether it was clicked.
fn side_row(ui: &mut egui::Ui, t: &Tokens, icon: &str, label: &str, count: usize, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, CornerRadius::same(6), t.selected);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), t.hover);
    }
    icons::paint(ui, egui::Rect::from_min_size(rect.min + vec2(6.0, 5.0), vec2(16.0, 16.0)), icon, 15.0, if selected { t.accent } else { t.icon });
    let color = if selected { t.text } else { t.text_muted };
    ui.painter().text(rect.left_center() + vec2(28.0, 0.0), egui::Align2::LEFT_CENTER, label, theme::regular(13.0), color);
    if count > 0 {
        ui.painter().text(rect.right_center() - vec2(8.0, 0.0), egui::Align2::RIGHT_CENTER, count.to_string(), theme::regular(11.5), t.text_faint);
    }
    theme::hand(resp)
}

fn sidebar(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let Some(lib) = app.library.lib.as_ref() else { return };
    let recent = (chrono::Utc::now() - chrono::Duration::days(14)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let count = |s: &Scope| lib.papers().filter(|p| s.admits(p, &recent)).count();
    let fixed: Vec<(Scope, &str, &str, usize)> = vec![
        (Scope::All, "files", "All papers", lib.len()),
        (Scope::Recent, "clock", "Recently added", count(&Scope::Recent)),
        (Scope::Favorites, "star", "Favorites", count(&Scope::Favorites)),
        (Scope::NeedsReview, "triangle-alert", "Needs review", count(&Scope::NeedsReview)),
        (Scope::Unfiled, "file-minus", "Unfiled", count(&Scope::Unfiled)),
    ];
    let states: Vec<(Scope, &str, &str, usize)> = vec![
        (Scope::State(ReadState::Unread), "circle", "Unread", count(&Scope::State(ReadState::Unread))),
        (Scope::State(ReadState::Reading), "book-open", "Reading", count(&Scope::State(ReadState::Reading))),
        (Scope::State(ReadState::Read), "circle-check", "Read", count(&Scope::State(ReadState::Read))),
    ];
    let mut collections: Vec<(CollectionId, String, usize, bool)> =
        lib.collections.iter().map(|c| (c.id, c.name.clone(), lib.collection_count(c.id), c.parent.is_some())).collect();
    // Children right under their parent.
    let order: Vec<CollectionId> = lib
        .collections
        .iter()
        .filter(|c| c.parent.is_none())
        .flat_map(|c| std::iter::once(c.id).chain(lib.collections.iter().filter(move |k| k.parent == Some(c.id)).map(|k| k.id)))
        .collect();
    collections.sort_by_key(|c| order.iter().position(|o| *o == c.0).unwrap_or(usize::MAX));
    let tags = lib.tags();

    let mut pick: Option<Scope> = None;
    egui::ScrollArea::vertical().id_salt("library_sidebar").auto_shrink([false, false]).show(ui, |ui| {
        ui.label(RichText::new("My Library").font(theme::semibold(16.0)).color(t.text));
        section_label(ui, t, "Library");
        for (scope, icon, label, n) in fixed.iter().chain(states.iter()) {
            if side_row(ui, t, icon, label, *n, app.library.filter.scope == *scope).clicked() {
                pick = Some(scope.clone());
            }
        }
        ui.horizontal(|ui| {
            section_label(ui, t, "Collections");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icons::button(ui, "folder-plus", 22.0, false, "New collection").clicked() {
                    app.library.new_collection = Some(String::new());
                }
            });
        });
        let mut remove = None;
        let mut new_child = None;
        for (id, name, n, child) in &collections {
            if let Some((rid, text)) = app.library.renaming.as_mut().filter(|(rid, _)| rid == id) {
                let resp = ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
                resp.request_focus();
                if resp.lost_focus() {
                    let (rid, text) = (*rid, text.clone());
                    if let Some(lib) = app.library.lib.as_mut() {
                        lib.rename_collection(rid, &text);
                    }
                    app.library.renaming = None;
                    app.library.last_edit = Some(Instant::now());
                }
                continue;
            }
            let resp = ui
                .horizontal(|ui| {
                    if *child {
                        ui.add_space(14.0);
                    }
                    side_row(ui, t, "folder", name, *n, app.library.filter.scope == Scope::Collection(*id))
                })
                .inner;
            // Papers dropped on a collection join it.
            if let Some(dropped) = resp.dnd_release_payload::<Vec<PaperId>>() {
                if let Some(lib) = app.library.lib.as_mut() {
                    for p in dropped.iter() {
                        lib.set_in_collection(*p, *id, true);
                    }
                }
                app.library.last_edit = Some(Instant::now());
            }
            if resp.clicked() {
                pick = Some(Scope::Collection(*id));
            }
            if resp.double_clicked() {
                app.library.renaming = Some((*id, name.clone()));
            }
            resp.context_menu(|ui| {
                if ui.button("Rename").clicked() {
                    app.library.renaming = Some((*id, name.clone()));
                    ui.close();
                }
                if !child && ui.button("New sub-collection").clicked() {
                    new_child = Some(*id);
                    ui.close();
                }
                if ui.button("Delete collection (papers stay)").clicked() {
                    remove = Some(*id);
                    ui.close();
                }
            });
        }
        if let Some(text) = app.library.new_collection.as_mut() {
            let resp = ui.add(egui::TextEdit::singleline(text).hint_text("Collection name").desired_width(f32::INFINITY));
            resp.request_focus();
            if resp.lost_focus() {
                let name = text.clone();
                app.library.new_collection = None;
                if !name.trim().is_empty()
                    && let Some(lib) = app.library.lib.as_mut()
                {
                    let id = lib.add_collection(&name, None);
                    pick = Some(Scope::Collection(id));
                    app.library.last_edit = Some(Instant::now());
                }
            }
        }
        if let Some(parent) = new_child
            && let Some(lib) = app.library.lib.as_mut()
        {
            let id = lib.add_collection("New collection", Some(parent));
            app.library.renaming = Some((id, "New collection".into()));
            app.library.last_edit = Some(Instant::now());
        }
        if let Some(id) = remove
            && let Some(lib) = app.library.lib.as_mut()
        {
            lib.remove_collection(id);
            if app.library.filter.scope == Scope::Collection(id) {
                pick = Some(Scope::All);
            }
            app.library.last_edit = Some(Instant::now());
        }
        if collections.is_empty() && app.library.new_collection.is_none() {
            ui.label(RichText::new("Group papers by project or topic.").font(theme::regular(11.5)).color(t.text_faint));
        }
        if !tags.is_empty() {
            section_label(ui, t, "Tags");
            for (tag, n) in &tags {
                let scope = Scope::Tag(tag.clone());
                if side_row(ui, t, "tag", tag, *n, app.library.filter.scope == scope).clicked() {
                    pick = Some(scope);
                }
            }
        }
        section_label(ui, t, "Watched folders");
        let watched = app.library.lib.as_ref().map(|l| l.settings.watched.clone()).unwrap_or_default();
        let mut unwatch = None;
        for (i, w) in watched.iter().enumerate() {
            let name = std::path::Path::new(w).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| w.clone());
            let resp = side_row(ui, t, "folder-open", &name, 0, false).on_hover_text(w);
            resp.context_menu(|ui| {
                if ui.button("Stop watching").clicked() {
                    unwatch = Some(i);
                    ui.close();
                }
            });
            if resp.clicked() {
                app.library_scan_watched();
            }
        }
        if let Some(i) = unwatch
            && let Some(lib) = app.library.lib.as_mut()
        {
            if i < lib.settings.watched.len() {
                lib.settings.watched.remove(i);
            }
            app.library.last_edit = Some(Instant::now());
        }
        if ui.add(egui::Button::new(RichText::new("+ Watch a folder…").font(theme::regular(12.0)).color(t.accent)).frame(false)).clicked() {
            let dialog = rfd::AsyncFileDialog::new().set_title("Choose a folder whose PDFs join the library");
            app.ask_one(crate::pickers::Ask::Folder(dialog), None, |app, dir| {
                app.library.ensure_loaded();
                if let Some(lib) = app.library.lib.as_mut() {
                    let d = dir.to_string_lossy().into_owned();
                    if !lib.settings.watched.contains(&d) {
                        lib.settings.watched.push(d);
                    }
                }
                app.library.last_edit = Some(Instant::now());
                app.library_scan_watched();
            });
        }
        ui.add_space(8.0);
        if let Some(lib) = app.library.lib.as_mut() {
            let mut auto = lib.settings.auto_lookup;
            if ui
                .add(crate::widgets::checkbox(&mut auto, RichText::new("Look up new papers online").font(theme::regular(12.0))))
                .on_hover_text("When a new PDF has a DOI or arXiv id, fill its details from CrossRef or arXiv. Only the identifier is sent.")
                .changed()
            {
                lib.settings.auto_lookup = auto;
                app.library.last_edit = Some(Instant::now());
            }
        }
    });
    if let Some(scope) = pick {
        app.library.filter.scope = scope;
    }
}

fn paper_list(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    // Toolbar.
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(RichText::new("Add PDFs…").font(theme::medium(13.0)).color(Color32::WHITE)).fill(t.accent).corner_radius(6))
            .clicked()
        {
            let dialog = rfd::AsyncFileDialog::new().add_filter("PDF", &["pdf", "PDF"]).set_title("Add PDFs to the library");
            app.ask(crate::pickers::Ask::Files(dialog), None, |app, paths| app.library_add_paths(paths));
        }
        if ui.button(RichText::new("Add folder…").font(theme::regular(13.0))).clicked() {
            let dialog = rfd::AsyncFileDialog::new().set_title("Add every PDF in a folder");
            app.ask_one(crate::pickers::Ask::Folder(dialog), None, |app, dir| app.library_add_paths(vec![dir]));
        }
        ui.add_space(8.0);
        let search = ui.add(
            egui::TextEdit::singleline(&mut app.library.filter.query)
                .hint_text("Search titles, authors, notes and full text")
                .desired_width((ui.available_width() - 110.0).max(120.0)),
        );
        if search.changed() {
            app.library.query_changed = Some(Instant::now());
            if app.library.filter.query.trim().is_empty() {
                app.library.fulltext_hits.clear();
            }
        }
        export_menu(app, ui);
    });
    ui.add_space(8.0);

    let Some(lib) = app.library.lib.as_ref() else { return };
    let hit_ids: Vec<PaperId> =
        if app.library.fulltext_query == app.library.filter.query { app.library.fulltext_hits.iter().map(|h| h.0).collect() } else { Vec::new() };
    let rows: Vec<RowData> = lib
        .list(&app.library.filter, app.library.sort, app.library.ascending, &hit_ids)
        .into_iter()
        .map(|p| RowData {
            id: p.id,
            favorite: p.favorite,
            title: p.display_title(),
            authors: p.meta.author_short(),
            year: p.meta.year.map(|y| y.to_string()).unwrap_or_default(),
            venue: p.meta.venue.clone(),
            added: p.added.get(..10).unwrap_or("").to_owned(),
            state: p.read_state,
            snippet: app.library.fulltext_hits.iter().find(|h| h.0 == p.id).map(|h| h.1.clone()),
            missing: !std::path::Path::new(&p.path).is_file(),
        })
        .collect();

    if lib.is_empty() {
        empty_state(app, ui, t);
        return;
    }
    if let Some(e) = &app.library.load_error {
        ui.label(RichText::new(format!("⚠ {e}")).color(Color32::from_rgb(0xD9, 0x6C, 0x00)).font(theme::regular(12.0)));
    }
    ui.label(RichText::new(format!("{} paper{}", rows.len(), if rows.len() == 1 { "" } else { "s" })).font(theme::regular(11.5)).color(t.text_faint));

    let mut open = None;
    let mut toggle_fav = None;
    let mut sort_by = None;
    let mut context: Option<(PaperId, ContextAction)> = None;
    let row_h = 30.0;
    let selected = app.library.selected.clone();
    egui_extras::TableBuilder::new(ui)
        .striped(false)
        .resizable(true)
        .sense(Sense::click_and_drag())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(egui_extras::Column::exact(24.0))
        .column(egui_extras::Column::remainder().at_least(120.0).clip(true))
        .column(egui_extras::Column::initial(110.0).at_least(50.0).clip(true))
        .column(egui_extras::Column::exact(40.0))
        .column(egui_extras::Column::initial(100.0).at_least(40.0).clip(true))
        .column(egui_extras::Column::exact(76.0).clip(true))
        .header(24.0, |mut h| {
            let heads = [
                ("", None),
                ("Title", Some(SortKey::Title)),
                ("Authors", Some(SortKey::Authors)),
                ("Year", Some(SortKey::Year)),
                ("Venue", Some(SortKey::Venue)),
                ("Added", Some(SortKey::Added)),
            ];
            for (label, key) in heads {
                h.col(|ui| {
                    let mark = match key {
                        Some(k) if k == app.library.sort => {
                            if app.library.ascending {
                                " ▲"
                            } else {
                                " ▼"
                            }
                        }
                        _ => "",
                    };
                    let resp = ui.add(
                        egui::Label::new(RichText::new(format!("{label}{mark}")).font(theme::semibold(11.5)).color(t.text_muted))
                            .sense(Sense::click()),
                    );
                    if let Some(k) = key
                        && resp.clicked()
                    {
                        sort_by = Some(k);
                    }
                });
            }
        })
        .body(|body| {
            body.heterogeneous_rows(rows.iter().map(|r| if r.snippet.is_some() { row_h + 18.0 } else { row_h }), |mut row| {
                let r = &rows[row.index()];
                row.set_selected(selected.contains(&r.id));
                row.col(|ui| {
                    let star = if r.favorite { "★" } else { "☆" };
                    let c = if r.favorite { Color32::from_rgb(0xF2, 0xB4, 0x00) } else { t.text_faint };
                    if ui.add(egui::Label::new(RichText::new(star).color(c).size(15.0)).sense(Sense::click())).clicked() {
                        toggle_fav = Some(r.id);
                    }
                });
                row.col(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            let dot = match r.state {
                                ReadState::Unread => Some(t.accent),
                                _ => None,
                            };
                            if let Some(c) = dot {
                                let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                                ui.painter().circle_filled(rect.center(), 3.5, c);
                            }
                            let mut title =
                                RichText::new(&r.title).font(if r.state == ReadState::Unread { theme::medium(13.0) } else { theme::regular(13.0) });
                            if r.missing {
                                title = title.strikethrough().color(t.text_faint);
                            }
                            ui.add(egui::Label::new(title).truncate());
                        });
                        if let Some(s) = &r.snippet {
                            ui.add(egui::Label::new(RichText::new(s).font(theme::regular(11.0)).color(t.text_faint).italics()).truncate());
                        }
                    });
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(RichText::new(&r.authors).font(theme::regular(12.5)).color(t.text_muted)).truncate());
                });
                row.col(|ui| {
                    ui.label(RichText::new(&r.year).font(theme::regular(12.5)).color(t.text_muted));
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(RichText::new(&r.venue).font(theme::regular(12.5)).color(t.text_muted).italics()).truncate());
                });
                row.col(|ui| {
                    ui.label(RichText::new(&r.added).font(theme::regular(12.0)).color(t.text_faint));
                });
                let resp = row.response();
                if resp.clicked() {
                    let (shift, cmd) = resp.ctx.input(|i| (i.modifiers.shift, i.modifiers.command));
                    select(&mut app.library.selected, &rows, r.id, shift, cmd);
                }
                if resp.double_clicked() {
                    open = Some(r.id);
                }
                if resp.drag_started() {
                    let ids = if selected.contains(&r.id) { selected.clone() } else { vec![r.id] };
                    resp.dnd_set_drag_payload(ids);
                }
                resp.context_menu(|ui| {
                    for (label, action) in [
                        ("Open", ContextAction::Open),
                        ("Look up details online", ContextAction::Lookup),
                        ("Re-read details from the PDF", ContextAction::Reread),
                        ("Copy citation (APA)", ContextAction::CopyApa),
                        ("Copy BibTeX", ContextAction::CopyBib),
                        ("Show in folder", ContextAction::Reveal),
                        ("Remove from library", ContextAction::Remove),
                    ] {
                        if ui.button(label).clicked() {
                            context = Some((r.id, action));
                            ui.close();
                        }
                    }
                });
            });
        });

    if let Some(k) = sort_by {
        if app.library.sort == k {
            app.library.ascending = !app.library.ascending;
        } else {
            app.library.sort = k;
            app.library.ascending = matches!(k, SortKey::Title | SortKey::Authors | SortKey::Venue);
        }
    }
    if let Some(id) = toggle_fav
        && let Some(p) = app.library.lib.as_mut().and_then(|l| l.get_mut(id))
    {
        p.favorite = !p.favorite;
        app.library.edited(id);
    }
    if let Some(id) = open {
        app.library_open_paper(id, None);
    }
    if let Some((id, action)) = context {
        context_action(app, ui, id, action);
    }
    // Delete removes the selected records (never the files).
    if ui.ctx().input(|i| i.key_pressed(egui::Key::Delete) || (i.modifiers.command && i.key_pressed(egui::Key::Backspace)))
        && ui.ctx().memory(|m| m.focused().is_none())
    {
        let ids = app.library.selected.clone();
        for id in ids {
            context_action(app, ui, id, ContextAction::Remove);
        }
    }
}

struct RowData {
    id: PaperId,
    favorite: bool,
    title: String,
    authors: String,
    year: String,
    venue: String,
    added: String,
    state: ReadState,
    snippet: Option<String>,
    missing: bool,
}

#[derive(Clone, Copy)]
enum ContextAction {
    Open,
    Lookup,
    Reread,
    CopyApa,
    CopyBib,
    Reveal,
    Remove,
}

fn context_action(app: &mut PdfCraftApp, ui: &egui::Ui, id: PaperId, action: ContextAction) {
    match action {
        ContextAction::Open => app.library_open_paper(id, None),
        ContextAction::Lookup => app.library_lookup(id, false),
        ContextAction::Reread => {
            // Start over from the file (and the online record): replaces the details, keeps
            // notes, tags, collections and reading state.
            if let Some(p) = app.library.lib.as_mut().and_then(|l| l.get_mut(id)) {
                p.meta = Metadata::default();
                p.lookup_tried = false;
            }
            app.library.drafts.paper = None;
            app.library_import(id);
        }
        ContextAction::CopyApa | ContextAction::CopyBib => {
            if let Some(p) = app.library.lib.as_ref().and_then(|l| l.get(id)) {
                let text = if matches!(action, ContextAction::CopyApa) { cite::apa(p) } else { cite::bibtex(&[p]) };
                ui.ctx().copy_text(text);
                app.notify("Copied");
            }
        }
        ContextAction::Reveal => {
            if let Some(p) = app.library.lib.as_ref().and_then(|l| l.get(id)) {
                let folder = std::path::Path::new(&p.path).parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
                ui.ctx().open_url(egui::OpenUrl::new_tab(format!("file://{folder}")));
            }
        }
        ContextAction::Remove => {
            if let Some(lib) = app.library.lib.as_mut() {
                lib.remove(id);
            }
            app.library.selected.retain(|s| *s != id);
            app.library.marks.remove(&id);
            app.library.last_edit = Some(Instant::now());
        }
    }
}

fn select(selected: &mut Vec<PaperId>, rows: &[RowData], id: PaperId, shift: bool, cmd: bool) {
    if cmd {
        if let Some(i) = selected.iter().position(|s| *s == id) {
            selected.remove(i);
        } else {
            selected.push(id);
        }
    } else if shift && let Some(anchor) = selected.first().copied() {
        let a = rows.iter().position(|r| r.id == anchor);
        let b = rows.iter().position(|r| r.id == id);
        if let (Some(a), Some(b)) = (a, b) {
            let (lo, hi) = (a.min(b), a.max(b));
            *selected = std::iter::once(anchor).chain(rows.get(lo..=hi).unwrap_or(&[]).iter().map(|r| r.id).filter(|r| *r != anchor)).collect();
        }
    } else {
        *selected = vec![id];
    }
}

fn empty_state(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    ui.add_space(60.0);
    ui.vertical_centered(|ui| {
        icons::paint(ui, egui::Rect::from_center_size(ui.cursor().center_top() + vec2(0.0, 24.0), vec2(48.0, 48.0)), "book-open", 44.0, t.text_faint);
        ui.add_space(56.0);
        ui.label(RichText::new("Your library is empty").font(theme::semibold(18.0)));
        ui.add_space(4.0);
        ui.label(
            RichText::new("Add PDFs or watch a folder. Titles, authors and DOIs are read from each paper\nand completed from CrossRef or arXiv when it has an identifier.")
                .font(theme::regular(13.0))
                .color(t.text_muted),
        );
        ui.add_space(14.0);
        if ui.add(egui::Button::new(RichText::new("Add PDFs…").font(theme::medium(13.0)).color(Color32::WHITE)).fill(t.accent).corner_radius(6)).clicked() {
            let dialog = rfd::AsyncFileDialog::new().add_filter("PDF", &["pdf", "PDF"]);
            app.ask(crate::pickers::Ask::Files(dialog), None, |app, paths| app.library_add_paths(paths));
        }
    });
}

fn export_menu(app: &mut PdfCraftApp, ui: &mut egui::Ui) {
    ui.menu_button(RichText::new("Export ▾").font(theme::regular(13.0)), |ui| {
        let Some(lib) = app.library.lib.as_ref() else { return };
        // The selection, or everything shown when nothing is selected.
        let ids: Vec<PaperId> = if app.library.selected.is_empty() {
            lib.list(&app.library.filter, app.library.sort, app.library.ascending, &[]).iter().map(|p| p.id).collect()
        } else {
            app.library.selected.clone()
        };
        let papers: Vec<&pdfcraft_library::Paper> = ids.iter().filter_map(|id| lib.get(*id)).collect();
        ui.label(RichText::new(format!("{} paper{}", papers.len(), if papers.len() == 1 { "" } else { "s" })).weak());
        let mut copy = None;
        let mut save: Option<(String, &str, &str)> = None;
        if ui.button("Copy APA references").clicked() {
            copy = Some(cite::apa_list(&papers));
        }
        if ui.button("Copy BibTeX").clicked() {
            copy = Some(cite::bibtex(&papers));
        }
        ui.separator();
        if ui.button("Save BibTeX (.bib)…").clicked() {
            save = Some((cite::bibtex(&papers), "references.bib", "bib"));
        }
        if ui.button("Save RIS (.ris)…").clicked() {
            save = Some((cite::ris(&papers), "references.ris", "ris"));
        }
        if let Some(text) = copy {
            ui.ctx().copy_text(text);
            app.notify("Copied");
            ui.close();
        }
        if let Some((text, name, ext)) = save {
            let dialog = rfd::AsyncFileDialog::new().set_file_name(name).add_filter(ext, &[ext]);
            app.ask_one(crate::pickers::Ask::Save(dialog), None, move |app, path| match std::fs::write(&path, text) {
                Ok(()) => app.notify(format!("Saved {}", path.display())),
                Err(e) => app.notify(format!("Couldn't save: {e}")),
            });
            ui.close();
        }
    });
}

fn details(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let Some(id) = app.library.selected.first().copied() else {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Select a paper to see its details, notes and highlights.").color(t.text_faint));
        });
        return;
    };
    if app.library.selected.len() > 1 {
        multi_details(app, ui, t);
        return;
    }
    let Some(paper) = app.library.lib.as_ref().and_then(|l| l.get(id)).cloned() else {
        app.library.selected.clear();
        return;
    };
    if app.library.drafts.paper != Some(id) {
        app.library.drafts = Drafts {
            paper: Some(id),
            authors: paper.meta.authors_text(),
            year: paper.meta.year.map(|y| y.to_string()).unwrap_or_default(),
            keywords: paper.meta.keywords.join(", "),
            new_tag: String::new(),
        };
    }

    // Header: title, authors, actions.
    ui.label(RichText::new(paper.display_title()).font(theme::semibold(16.0)).color(t.text));
    let byline = [paper.meta.author_short(), paper.meta.year.map(|y| y.to_string()).unwrap_or_default(), paper.meta.venue.clone()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if !byline.is_empty() {
        ui.label(RichText::new(byline).font(theme::regular(12.5)).color(t.text_muted));
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(RichText::new("Open PDF").font(theme::medium(12.5)).color(Color32::WHITE)).fill(t.accent).corner_radius(6))
            .clicked()
        {
            app.library_open_paper(id, None);
        }
        let busy = app.library.busy.contains(&id);
        let can_lookup = !paper.meta.doi.trim().is_empty() || !paper.meta.arxiv.trim().is_empty() || paper.meta.title.split_whitespace().count() >= 3;
        let label = if busy { "Working…" } else { "Look up" };
        if ui
            .add_enabled(!busy && can_lookup, egui::Button::new(RichText::new(label).font(theme::regular(12.5))))
            .on_hover_text("Fill empty fields from CrossRef (by DOI, or by searching the title) or arXiv. Shift-click to replace every field.")
            .on_disabled_hover_text("Add a title, DOI or arXiv id first")
            .clicked()
        {
            let overwrite = ui.input(|i| i.modifiers.shift);
            app.library_lookup(id, overwrite);
        }
        if busy {
            ui.spinner();
        }
        let mut state = paper.read_state;
        egui::ComboBox::from_id_salt("read_state").selected_text(state.label()).width(80.0).show_ui(ui, |ui| {
            for s in ReadState::ALL {
                ui.selectable_value(&mut state, s, s.label());
            }
        });
        if state != paper.read_state
            && let Some(p) = app.library.lib.as_mut().and_then(|l| l.get_mut(id))
        {
            p.read_state = state;
            app.library.edited(id);
        }
    });
    ui.add_space(8.0);

    // Tabs.
    let n_marks = app.library.marks.get(&id).map(Vec::len);
    ui.horizontal(|ui| {
        for (tab, label) in [
            (DetailTab::Info, "Info".to_owned()),
            (DetailTab::Notes, "Notes".to_owned()),
            (
                DetailTab::Highlights,
                match n_marks {
                    Some(n) if n > 0 => format!("Highlights ({n})"),
                    _ => "Highlights".to_owned(),
                },
            ),
        ] {
            let on = app.library.detail == tab;
            let text = RichText::new(label).font(if on { theme::semibold(13.0) } else { theme::regular(13.0) }).color(if on {
                t.text
            } else {
                t.text_muted
            });
            let resp = ui.add(egui::Button::new(text).frame(false));
            if on {
                let r = resp.rect;
                ui.painter().hline(r.x_range(), r.bottom() + 3.0, Stroke::new(2.0, t.accent));
            }
            if resp.clicked() {
                app.library.detail = tab;
            }
        }
    });
    ui.separator();
    match app.library.detail {
        DetailTab::Info => info_tab(app, ui, t, id),
        DetailTab::Notes => notes_tab(app, ui, t, id),
        DetailTab::Highlights => highlights_tab(app, ui, t, id),
    }
}

fn multi_details(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let ids = app.library.selected.clone();
    ui.label(RichText::new(format!("{} papers selected", ids.len())).font(theme::semibold(16.0)));
    ui.label(RichText::new("Drag them onto a collection, tag them all, or export them from Export ▾.").color(t.text_muted));
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label("Tag all:");
        let resp = ui.add(egui::TextEdit::singleline(&mut app.library.drafts.new_tag).hint_text("tag").desired_width(140.0));
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let tag = std::mem::take(&mut app.library.drafts.new_tag);
            if let Some(lib) = app.library.lib.as_mut() {
                for id in &ids {
                    lib.add_tag(*id, &tag);
                }
            }
            app.library.last_edit = Some(Instant::now());
        }
    });
    let collections: Vec<(CollectionId, String)> =
        app.library.lib.as_ref().map(|l| l.collections.iter().map(|c| (c.id, c.name.clone())).collect()).unwrap_or_default();
    if !collections.is_empty() {
        ui.menu_button("Add to collection ▾", |ui| {
            for (cid, name) in &collections {
                if ui.button(name).clicked() {
                    if let Some(lib) = app.library.lib.as_mut() {
                        for id in &ids {
                            lib.set_in_collection(*id, *cid, true);
                        }
                    }
                    app.library.last_edit = Some(Instant::now());
                    ui.close();
                }
            }
        });
    }
    ui.horizontal(|ui| {
        if ui.button("Mark as read").clicked() {
            for id in &ids {
                if let Some(p) = app.library.lib.as_mut().and_then(|l| l.get_mut(*id)) {
                    p.read_state = ReadState::Read;
                }
                app.library.edited(*id);
            }
        }
        if ui.button("Look up all").clicked() {
            for id in &ids {
                app.library_lookup(*id, false);
            }
        }
    });
}

/// A labelled one-line field bound to a string; returns whether it changed.
fn field(ui: &mut egui::Ui, t: &Tokens, label: &str, value: &mut String) -> bool {
    ui.label(RichText::new(label).font(theme::regular(11.5)).color(t.text_faint));
    ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY)).changed()
}

fn info_tab(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, id: PaperId) {
    let collections: Vec<(CollectionId, String)> =
        app.library.lib.as_ref().map(|l| l.collections.iter().map(|c| (c.id, c.name.clone())).collect()).unwrap_or_default();
    egui::ScrollArea::vertical().id_salt("library_info").auto_shrink([false, false]).show(ui, |ui| {
        let drafts = &mut app.library.drafts;
        let Some(lib) = app.library.lib.as_mut() else { return };
        let Some(p) = lib.get_mut(id) else { return };
        let mut changed = false;
        let m = &mut p.meta;

        ui.label(RichText::new("Type").font(theme::regular(11.5)).color(t.text_faint));
        const KINDS: [(&str, &str); 9] = [
            ("article", "Journal article"),
            ("inproceedings", "Conference paper"),
            ("book", "Book"),
            ("incollection", "Book chapter"),
            ("phdthesis", "Thesis"),
            ("techreport", "Report"),
            ("misc", "Preprint / other"),
            ("mastersthesis", "Master's thesis"),
            ("unpublished", "Unpublished"),
        ];
        let shown = KINDS.iter().find(|k| k.0 == m.kind).map(|k| k.1).unwrap_or("Journal article");
        egui::ComboBox::from_id_salt("paper_kind").selected_text(shown).width(ui.available_width()).show_ui(ui, |ui| {
            for (k, label) in KINDS {
                if ui.selectable_label(m.kind == k, label).clicked() {
                    m.kind = k.to_owned();
                    changed = true;
                }
            }
        });
        ui.label(RichText::new("Title").font(theme::regular(11.5)).color(t.text_faint));
        changed |= ui.add(egui::TextEdit::multiline(&mut m.title).desired_rows(2).desired_width(f32::INFINITY)).changed();
        ui.label(RichText::new("Authors (Family, Given; …)").font(theme::regular(11.5)).color(t.text_faint));
        if ui.add(egui::TextEdit::multiline(&mut drafts.authors).desired_rows(2).desired_width(f32::INFINITY)).changed() {
            m.authors = Author::parse_list(&drafts.authors);
            changed = true;
        }
        ui.columns(2, |cols| {
            cols[0].label(RichText::new("Year").font(theme::regular(11.5)).color(t.text_faint));
            if cols[0].add(egui::TextEdit::singleline(&mut drafts.year).desired_width(f32::INFINITY)).changed() {
                m.year = drafts.year.trim().parse::<i32>().ok().filter(|y| (0..=3000).contains(y));
                changed = true;
            }
            changed |= field(&mut cols[1], t, "Pages", &mut m.pages);
        });
        changed |= field(ui, t, "Journal / proceedings", &mut m.venue);
        ui.columns(2, |cols| {
            changed |= field(&mut cols[0], t, "Volume", &mut m.volume);
            changed |= field(&mut cols[1], t, "Issue", &mut m.issue);
        });
        changed |= field(ui, t, "Publisher", &mut m.publisher);
        changed |= field(ui, t, "DOI", &mut m.doi);
        ui.columns(2, |cols| {
            changed |= field(&mut cols[0], t, "arXiv id", &mut m.arxiv);
            changed |= field(&mut cols[1], t, "ISBN", &mut m.isbn);
        });
        changed |= field(ui, t, "URL", &mut m.url);
        ui.label(RichText::new("Abstract").font(theme::regular(11.5)).color(t.text_faint));
        changed |= ui.add(egui::TextEdit::multiline(&mut m.abstract_text).desired_rows(5).desired_width(f32::INFINITY)).changed();
        ui.label(RichText::new("Keywords (comma-separated)").font(theme::regular(11.5)).color(t.text_faint));
        if ui.add(egui::TextEdit::singleline(&mut drafts.keywords).desired_width(f32::INFINITY)).changed() {
            m.keywords = drafts.keywords.split(',').map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned).collect();
            changed = true;
        }
        let auto_key = {
            let mut probe = p.clone();
            probe.cite_key.clear();
            cite::generated_key(&probe)
        };
        ui.label(RichText::new("Citation key").font(theme::regular(11.5)).color(t.text_faint));
        changed |= ui.add(egui::TextEdit::singleline(&mut p.cite_key).hint_text(auto_key).desired_width(f32::INFINITY)).changed();

        // Tags.
        ui.add_space(8.0);
        ui.label(RichText::new("Tags").font(theme::regular(11.5)).color(t.text_faint));
        let mut remove_tag = None;
        ui.horizontal_wrapped(|ui| {
            for tag in &p.tags {
                let resp =
                    ui.add(egui::Button::new(RichText::new(format!("{tag}  ×")).font(theme::regular(12.0))).corner_radius(10).fill(t.accent_soft));
                if resp.on_hover_text("Remove tag").clicked() {
                    remove_tag = Some(tag.clone());
                }
            }
            let resp = ui.add(egui::TextEdit::singleline(&mut drafts.new_tag).hint_text("add tag").desired_width(90.0));
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let tag = std::mem::take(&mut drafts.new_tag);
                let tag = tag.trim().trim_start_matches('#').trim();
                if !tag.is_empty() && !p.has_tag(tag) {
                    p.tags.push(tag.to_owned());
                    changed = true;
                }
                resp.request_focus();
            }
        });
        if let Some(tag) = remove_tag {
            p.tags.retain(|x| *x != tag);
            changed = true;
        }

        // Collections.
        if !collections.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Collections").font(theme::regular(11.5)).color(t.text_faint));
            for (cid, name) in &collections {
                let mut member = p.collections.contains(cid);
                if ui.add(crate::widgets::checkbox(&mut member, name.as_str())).changed() {
                    if member {
                        p.collections.push(*cid);
                    } else {
                        p.collections.retain(|c| c != cid);
                    }
                    changed = true;
                }
            }
        }
        ui.add_space(8.0);
        ui.label(RichText::new("File").font(theme::regular(11.5)).color(t.text_faint));
        ui.add(egui::Label::new(RichText::new(&p.path).font(theme::regular(11.5)).color(t.text_muted)).wrap());
        if let Some(dup) = lib.find_doi(&lib.get(id).map(|p| p.meta.doi.clone()).unwrap_or_default(), id) {
            ui.add_space(6.0);
            let other = lib.get(dup).map(|p| p.display_title()).unwrap_or_default();
            ui.label(
                RichText::new(format!("⚠ Same DOI as “{other}” — possibly a duplicate."))
                    .font(theme::regular(12.0))
                    .color(Color32::from_rgb(0xD9, 0x6C, 0x00)),
            );
        }
        if changed {
            lib.touch(id);
            app.library.last_edit = Some(Instant::now());
        }
    });
}

fn notes_tab(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, id: PaperId) {
    ui.horizontal(|ui| {
        if ui.button("Insert highlights").on_hover_text("Append this paper's highlights and their notes as Markdown").clicked() {
            match app.library.marks.get(&id) {
                Some(marks) => {
                    let digest = highlights_markdown(marks, app.library.marks_by_color);
                    if let Some(p) = app.library.lib.as_mut().and_then(|l| l.get_mut(id)) {
                        if !p.notes.trim().is_empty() {
                            p.notes.push_str("\n\n");
                        }
                        p.notes.push_str(&digest);
                    }
                    app.library.edited(id);
                }
                None => app.library_load_marks(id),
            }
        }
        if ui.button("Copy as Markdown").on_hover_text("The reference, your notes and all highlights").clicked()
            && let Some(p) = app.library.lib.as_ref().and_then(|l| l.get(id))
        {
            let marks: Vec<_> = app.library.marks.get(&id).map(|m| m.iter().map(|m| m.note.clone()).collect()).unwrap_or_default();
            ui.ctx().copy_text(cite::notes_markdown(p, &marks, app.library.marks_by_color));
            app.notify("Copied");
        }
    });
    if !app.library.marks.contains_key(&id) && !app.library.marks_loading.contains(&id) {
        app.library_load_marks(id);
    }
    ui.add_space(4.0);
    let Some(p) = app.library.lib.as_mut().and_then(|l| l.get_mut(id)) else { return };
    let changed = egui::ScrollArea::vertical().id_salt("library_notes").auto_shrink([false, false]).show(ui, |ui| {
        ui.add(
            egui::TextEdit::multiline(&mut p.notes)
                .hint_text("Your notes on this paper: summary, arguments, how it relates to your work… (Markdown)")
                .font(theme::regular(13.5))
                .desired_rows(24)
                .desired_width(f32::INFINITY),
        )
        .changed()
    });
    if changed.inner {
        app.library.edited(id);
    }
    let _ = t;
}

/// The highlights alone as Markdown (for appending to notes).
fn highlights_markdown(marks: &[Mark], by_color: bool) -> String {
    let notes: Vec<_> = marks.iter().map(|m| m.note.clone()).collect();
    let empty = pdfcraft_library::Paper {
        id: PaperId(0),
        path: String::new(),
        file_size: 0,
        meta: Metadata::default(),
        tags: Vec::new(),
        collections: Vec::new(),
        notes: String::new(),
        read_state: ReadState::Unread,
        favorite: false,
        added: String::new(),
        modified: String::new(),
        last_opened: String::new(),
        cite_key: String::new(),
        lookup_tried: false,
    };
    let md = cite::notes_markdown(&empty, &notes, by_color);
    md.find("## Highlights").and_then(|i| md.get(i..)).unwrap_or("").to_owned()
}

fn highlights_tab(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, id: PaperId) {
    if !app.library.marks.contains_key(&id) && !app.library.marks_loading.contains(&id) {
        app.library_load_marks(id);
    }
    if app.library.marks_loading.contains(&id) && !app.library.marks.contains_key(&id) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Reading highlights…");
        });
        return;
    }
    let marks = app.library.marks.get(&id).cloned().unwrap_or_default();
    ui.horizontal(|ui| {
        if ui.button("Refresh").on_hover_text("Read the highlights again (after highlighting in the open document)").clicked() {
            app.library_load_marks(id);
        }
        if ui.button("Add to notes").clicked() {
            let digest = highlights_markdown(&marks, app.library.marks_by_color);
            if let Some(p) = app.library.lib.as_mut().and_then(|l| l.get_mut(id)) {
                if !p.notes.trim().is_empty() {
                    p.notes.push_str("\n\n");
                }
                p.notes.push_str(&digest);
            }
            app.library.edited(id);
            app.library.detail = DetailTab::Notes;
        }
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(highlights_markdown(&marks, app.library.marks_by_color));
            app.notify("Copied");
        }
        ui.add(crate::widgets::checkbox(&mut app.library.marks_by_color, "By colour"));
    });
    if marks.is_empty() {
        ui.add_space(20.0);
        ui.label(
            RichText::new("No highlights yet. Open the PDF, pick Highlight (the marker in the comment tools), choose a colour and select text. Click a highlight to add a note to it.")
                .color(t.text_faint),
        );
        return;
    }
    // Colour chips with counts: click to show one colour.
    let mut colors: Vec<(String, [f32; 3], usize)> = Vec::new();
    for m in &marks {
        match colors.iter_mut().find(|c| c.0 == m.note.color) {
            Some(c) => c.2 += 1,
            None => colors.push((m.note.color.clone(), m.rgb, 1)),
        }
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        let all = app.library.color_filter.is_none();
        if ui.selectable_label(all, format!("All {}", marks.len())).clicked() {
            app.library.color_filter = None;
        }
        for (name, rgb, n) in &colors {
            let on = app.library.color_filter.as_deref() == Some(name.as_str());
            let (rect, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
            ui.painter().circle_filled(rect.center(), 6.0, crate::comments::color32([f64::from(rgb[0]), f64::from(rgb[1]), f64::from(rgb[2])]));
            if on {
                ui.painter().circle_stroke(rect.center(), 7.5, Stroke::new(1.5, t.text));
            }
            let label = ui.selectable_label(on, format!("{name} {n}"));
            if resp.clicked() || label.clicked() {
                app.library.color_filter = if on { None } else { Some(name.clone()) };
            }
        }
    });
    ui.add_space(6.0);
    let mut jump = None;
    egui::ScrollArea::vertical().id_salt("library_marks").auto_shrink([false, false]).show(ui, |ui| {
        for m in marks.iter().filter(|m| app.library.color_filter.as_deref().is_none_or(|c| c == m.note.color)) {
            let c = crate::comments::color32([f64::from(m.rgb[0]), f64::from(m.rgb[1]), f64::from(m.rgb[2])]);
            let frame = egui::Frame::NONE.fill(t.panel).corner_radius(6).inner_margin(egui::Margin { left: 12, right: 8, top: 6, bottom: 6 });
            let resp = frame
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let kind = match m.note.kind.as_str() {
                            "Text" => "Note",
                            "FreeText" => "Text box",
                            k => k,
                        };
                        ui.label(RichText::new(format!("p. {} · {kind}", m.note.page + 1)).font(theme::regular(11.0)).color(t.text_faint));
                    });
                    if !m.note.text.is_empty() {
                        ui.label(RichText::new(format!("“{}”", m.note.text)).font(theme::regular(13.0)).color(t.text));
                    }
                    if !m.note.note.is_empty() {
                        ui.horizontal_top(|ui| {
                            icons::paint(ui, egui::Rect::from_min_size(ui.cursor().min, vec2(14.0, 14.0)), "sticky-note", 13.0, t.text_muted);
                            ui.add_space(16.0);
                            ui.label(RichText::new(&m.note.note).font(theme::regular(12.5)).color(t.text_muted));
                        });
                    }
                })
                .response;
            // The colour bar down the card's left edge.
            let bar = egui::Rect::from_min_size(resp.rect.min, vec2(4.0, resp.rect.height()));
            ui.painter().rect_filled(bar, CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 }, c);
            let resp = ui.interact(resp.rect, ui.id().with(("mark", m.note.page, &m.note.text, &m.note.note)), Sense::click());
            if resp.hovered() {
                ui.painter().rect_stroke(resp.rect, 6.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
            }
            if theme::hand(resp).on_hover_text("Open at this page").clicked() {
                jump = Some(m.note.page);
            }
            ui.add_space(6.0);
        }
    });
    if let Some(page) = jump {
        app.library_open_paper(id, Some(page));
    }
}

// ---- notes beside an open document ---------------------------------------------------------------

/// The floating "Paper notes" window, shown over a document that is in the library.
pub fn notes_window(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if !app.library.notes_window {
        return;
    }
    let Some(path) = app.active.and_then(|i| app.views.get(i)).and_then(|v| app.session.get(v.id)).and_then(|d| d.path.clone()) else { return };
    let Some(id) = app.library.paper_for(&path) else { return };
    let mut open = true;
    let title = app.library.lib.as_ref().and_then(|l| l.get(id)).map(|p| p.display_title()).unwrap_or_default();
    egui::Window::new("Paper notes").id(egui::Id::new("library_notes_window")).open(&mut open).default_size(vec2(360.0, 460.0)).resizable(true).show(
        ctx,
        |ui| {
            ui.label(RichText::new(title).strong());
            ui.add_space(4.0);
            let Some(p) = app.library.lib.as_mut().and_then(|l| l.get_mut(id)) else { return };
            let changed = egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut p.notes)
                            .hint_text("Notes on this paper (Markdown)")
                            .desired_rows(20)
                            .desired_width(f32::INFINITY),
                    )
                    .changed()
                })
                .inner;
            if changed {
                app.library.edited(id);
            }
        },
    );
    if !open {
        app.library.notes_window = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(ids: &[u64]) -> Vec<RowData> {
        ids.iter()
            .map(|i| RowData {
                id: PaperId(*i),
                favorite: false,
                title: String::new(),
                authors: String::new(),
                year: String::new(),
                venue: String::new(),
                added: String::new(),
                state: ReadState::Unread,
                snippet: None,
                missing: false,
            })
            .collect()
    }

    #[test]
    fn selection_click_cmd_shift() {
        let r = rows(&[1, 2, 3, 4]);
        let mut sel = Vec::new();
        select(&mut sel, &r, PaperId(2), false, false);
        assert_eq!(sel, vec![PaperId(2)]);
        select(&mut sel, &r, PaperId(4), true, false);
        assert_eq!(sel, vec![PaperId(2), PaperId(3), PaperId(4)]);
        select(&mut sel, &r, PaperId(3), false, true);
        assert_eq!(sel, vec![PaperId(2), PaperId(4)]);
        select(&mut sel, &r, PaperId(9), true, false);
        assert_eq!(sel, vec![PaperId(2), PaperId(4)], "an id not shown changes nothing");
    }

    #[test]
    fn highlight_digest_has_no_reference_header() {
        let marks = vec![Mark {
            note: pdfcraft_library::cite::HighlightNote {
                page: 1,
                kind: "Highlight".into(),
                color: "Yellow".into(),
                text: "x".into(),
                note: "y".into(),
            },
            rgb: [1.0, 1.0, 0.0],
        }];
        let md = highlights_markdown(&marks, true);
        assert!(md.starts_with("## Highlights"));
        assert!(md.contains("> x (p. 2)"));
        assert_eq!(highlights_markdown(&[], true), "");
    }
}
