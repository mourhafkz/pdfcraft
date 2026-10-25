//! The library on disk: `library.json` (papers, collections, settings) and a `fulltext/` folder
//! with each paper's extracted text, written atomically (temp file, then rename) so a crash
//! never leaves a half-written library.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{Collection, CollectionId, Paper, PaperId};
use crate::search::{self, Filter, SortKey};
use crate::{Error, Result};

/// The largest library file read (a library of tens of thousands of papers fits easily).
const MAX_FILE: u64 = 256 << 20;
/// The most full text kept per paper.
pub const MAX_FULLTEXT: usize = 2 << 20;
/// Files looked at per watched folder per scan, and how deep it goes.
const MAX_SCAN_ENTRIES: usize = 20_000;
const MAX_SCAN_DEPTH: usize = 6;

/// Settings kept with the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Folders whose PDFs join the library automatically (Mendeley's watched folders).
    pub watched: Vec<String>,
    /// Look new papers up on CrossRef/arXiv when they have a DOI or arXiv id.
    pub auto_lookup: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { watched: Vec::new(), auto_lookup: true }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    version: u32,
    next_id: u64,
    papers: Vec<Paper>,
    collections: Vec<Collection>,
    settings: Settings,
}

/// A library loaded in memory.
#[derive(Debug)]
pub struct Library {
    dir: PathBuf,
    next_id: u64,
    papers: BTreeMap<PaperId, Paper>,
    pub collections: Vec<Collection>,
    pub settings: Settings,
    dirty: bool,
}

/// The current time as RFC 3339 (seconds).
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Library {
    /// An empty library that will save to `dir`.
    pub fn empty(dir: impl Into<PathBuf>) -> Self {
        Library { dir: dir.into(), next_id: 1, papers: BTreeMap::new(), collections: Vec::new(), settings: Settings::default(), dirty: false }
    }

    /// Load the library in `dir`, or start an empty one if there is none yet. A damaged file is
    /// moved aside (`library.json.damaged-<time>`) rather than overwritten, and an empty library
    /// starts; the error says so.
    pub fn open(dir: impl Into<PathBuf>) -> (Self, Option<Error>) {
        let dir = dir.into();
        let path = dir.join("library.json");
        let text = match std::fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Library::empty(dir), None),
            Err(e) => return (Library::empty(dir), Some(e.into())),
            Ok(m) if m.len() > MAX_FILE => return (Library::empty(dir), Some(Error::Format("the library file is too large".into()))),
            Ok(_) => match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => return (Library::empty(dir), Some(e.into())),
            },
        };
        match serde_json::from_str::<File>(&text) {
            Ok(f) => {
                let mut papers = BTreeMap::new();
                let mut max_id = 0;
                for p in f.papers {
                    max_id = max_id.max(p.id.0);
                    papers.insert(p.id, p);
                }
                let max_col = f.collections.iter().map(|c| c.id.0).max().unwrap_or(0);
                let next_id = f.next_id.max(max_id.saturating_add(1)).max(max_col.saturating_add(1)).max(1);
                (Library { dir, next_id, papers, collections: f.collections, settings: f.settings, dirty: false }, None)
            }
            Err(e) => {
                let aside = dir.join(format!("library.json.damaged-{}", chrono::Utc::now().format("%Y%m%d%H%M%S")));
                let moved = std::fs::rename(&path, &aside).is_ok();
                let note = if moved { format!("{e}; it was kept as {}", aside.display()) } else { e.to_string() };
                (Library::empty(dir), Some(Error::Format(note)))
            }
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether there are changes not yet saved.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the library changed (callers that edit through [`Library::get_mut`] call this).
    pub fn touch(&mut self, id: PaperId) {
        if let Some(p) = self.papers.get_mut(&id) {
            p.modified = now();
        }
        self.dirty = true;
    }

    /// Write the library if it changed.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let file = File {
            version: 1,
            next_id: self.next_id,
            papers: self.papers.values().cloned().collect(),
            collections: self.collections.clone(),
            settings: self.settings.clone(),
        };
        let text = serde_json::to_string_pretty(&file).map_err(|e| Error::Format(e.to_string()))?;
        write_atomic(&self.dir.join("library.json"), text.as_bytes())?;
        self.dirty = false;
        Ok(())
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    pub fn len(&self) -> usize {
        self.papers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.papers.is_empty()
    }

    pub fn papers(&self) -> impl Iterator<Item = &Paper> {
        self.papers.values()
    }

    pub fn get(&self, id: PaperId) -> Option<&Paper> {
        self.papers.get(&id)
    }

    /// Edit a paper; call [`Library::touch`] afterwards.
    pub fn get_mut(&mut self, id: PaperId) -> Option<&mut Paper> {
        self.papers.get_mut(&id)
    }

    /// The paper for a file, compared as canonical paths when both resolve.
    pub fn find_path(&self, path: &str) -> Option<PaperId> {
        // Stored paths are canonical (see `add`), so only the query is resolved: no file-system
        // call per paper, which matters when the interface asks every frame.
        let canon = canonical(path);
        self.papers.values().find(|p| p.path == path || canon.as_deref() == Some(p.path.as_str())).map(|p| p.id)
    }

    /// Another paper with the same DOI (a likely duplicate).
    pub fn find_doi(&self, doi: &str, except: PaperId) -> Option<PaperId> {
        let doi = doi.trim();
        if doi.is_empty() {
            return None;
        }
        self.papers.values().find(|p| p.id != except && p.meta.doi.eq_ignore_ascii_case(doi)).map(|p| p.id)
    }

    /// Add a PDF. Returns its id and whether it is new (an existing record for the same file is
    /// returned as it is).
    pub fn add(&mut self, path: &str) -> (PaperId, bool) {
        if let Some(id) = self.find_path(path) {
            return (id, false);
        }
        let id = PaperId(self.fresh_id());
        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let stamp = now();
        let path = canonical(path).unwrap_or_else(|| path.to_owned());
        self.papers.insert(
            id,
            Paper {
                id,
                path,
                file_size,
                meta: Default::default(),
                tags: Vec::new(),
                collections: Vec::new(),
                notes: String::new(),
                read_state: Default::default(),
                favorite: false,
                added: stamp.clone(),
                modified: stamp,
                last_opened: String::new(),
                cite_key: String::new(),
                lookup_tried: false,
            },
        );
        self.dirty = true;
        (id, true)
    }

    /// Remove a paper's record (never its PDF) and its stored text.
    pub fn remove(&mut self, id: PaperId) -> Option<Paper> {
        let p = self.papers.remove(&id)?;
        let _ = std::fs::remove_file(self.fulltext_path(id));
        self.dirty = true;
        Some(p)
    }

    // ---- collections ---------------------------------------------------------------------------

    pub fn add_collection(&mut self, name: &str, parent: Option<CollectionId>) -> CollectionId {
        let id = CollectionId(self.fresh_id());
        let name = if name.trim().is_empty() { "New collection" } else { name.trim() };
        self.collections.push(Collection { id, name: name.to_owned(), parent: parent.filter(|p| self.collection(*p).is_some()) });
        self.dirty = true;
        id
    }

    pub fn collection(&self, id: CollectionId) -> Option<&Collection> {
        self.collections.iter().find(|c| c.id == id)
    }

    pub fn rename_collection(&mut self, id: CollectionId, name: &str) {
        if let Some(c) = self.collections.iter_mut().find(|c| c.id == id)
            && !name.trim().is_empty()
        {
            name.trim().clone_into(&mut c.name);
            self.dirty = true;
        }
    }

    /// Delete a collection and its sub-collections; their papers stay in the library.
    pub fn remove_collection(&mut self, id: CollectionId) {
        let doomed: Vec<CollectionId> = self.collections.iter().filter(|c| c.id == id || c.parent == Some(id)).map(|c| c.id).collect();
        self.collections.retain(|c| !doomed.contains(&c.id));
        for p in self.papers.values_mut() {
            p.collections.retain(|c| !doomed.contains(c));
        }
        self.dirty = true;
    }

    pub fn set_in_collection(&mut self, paper: PaperId, collection: CollectionId, member: bool) {
        if self.collection(collection).is_none() {
            return;
        }
        if let Some(p) = self.papers.get_mut(&paper) {
            let has = p.collections.contains(&collection);
            if member && !has {
                p.collections.push(collection);
            } else if !member && has {
                p.collections.retain(|c| *c != collection);
            } else {
                return;
            }
            self.dirty = true;
        }
    }

    /// Papers in each collection.
    pub fn collection_count(&self, id: CollectionId) -> usize {
        self.papers.values().filter(|p| p.collections.contains(&id)).count()
    }

    // ---- tags ----------------------------------------------------------------------------------

    /// Every tag in use with how many papers carry it, sorted case-insensitively.
    pub fn tags(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, (String, usize)> = BTreeMap::new();
        for t in self.papers.values().flat_map(|p| p.tags.iter()) {
            let e = counts.entry(search::fold(t)).or_insert_with(|| (t.clone(), 0));
            e.1 = e.1.saturating_add(1);
        }
        counts.into_values().collect()
    }

    pub fn add_tag(&mut self, paper: PaperId, tag: &str) {
        let tag = tag.trim().trim_start_matches('#').trim();
        if tag.is_empty() {
            return;
        }
        if let Some(p) = self.papers.get_mut(&paper)
            && !p.has_tag(tag)
        {
            p.tags.push(tag.to_owned());
            p.modified = now();
            self.dirty = true;
        }
    }

    pub fn remove_tag(&mut self, paper: PaperId, tag: &str) {
        if let Some(p) = self.papers.get_mut(&paper) {
            let before = p.tags.len();
            p.tags.retain(|t| !t.eq_ignore_ascii_case(tag));
            if p.tags.len() != before {
                p.modified = now();
                self.dirty = true;
            }
        }
    }

    // ---- listing -------------------------------------------------------------------------------

    /// The papers `filter` admits, sorted. Searches metadata and notes; full-text hits are added
    /// by the caller from [`Library::fulltext_hits`].
    pub fn list(&self, filter: &Filter, sort: SortKey, ascending: bool, extra_hits: &[PaperId]) -> Vec<&Paper> {
        let recent = (chrono::Utc::now() - chrono::Duration::days(14)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let terms = search::terms(&filter.query);
        let mut out: Vec<&Paper> = self
            .papers
            .values()
            .filter(|p| filter.scope.admits(p, &recent))
            .filter(|p| terms.is_empty() || search::all_terms_in(&terms, &search::haystack(p)) || extra_hits.contains(&p.id))
            .collect();
        search::sort(&mut out, sort, ascending);
        out
    }

    // ---- full text -----------------------------------------------------------------------------

    pub fn fulltext_path(&self, id: PaperId) -> PathBuf {
        self.dir.join("fulltext").join(format!("{}.txt", id.0))
    }

    /// Store a paper's extracted text (capped) for library-wide search.
    pub fn set_fulltext(&self, id: PaperId, text: &str) -> Result<()> {
        Self::store_fulltext(&self.dir, id, text)
    }

    /// [`Library::set_fulltext`] for a library in `dir`, from a worker thread.
    pub fn store_fulltext(dir: &Path, id: PaperId, text: &str) -> Result<()> {
        let mut end = text.len().min(MAX_FULLTEXT);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let path = dir.join("fulltext").join(format!("{}.txt", id.0));
        write_atomic(&path, text.get(..end).unwrap_or("").as_bytes())
    }

    pub fn has_fulltext(&self, id: PaperId) -> bool {
        self.fulltext_path(id).is_file()
    }

    /// The papers whose stored text contains every word of `query`, with an excerpt around the
    /// first hit. Reads the text files, so callers run it off the interface thread for big
    /// libraries. `ids` lists the papers to look at (paths are resolved here).
    pub fn fulltext_hits(dir: &Path, ids: &[PaperId], query: &str) -> Vec<(PaperId, String)> {
        let terms = search::terms(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for id in ids {
            let path = dir.join("fulltext").join(format!("{}.txt", id.0));
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if meta.len() > (MAX_FULLTEXT as u64).saturating_mul(2) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if search::all_terms_in(&terms, &search::fold(&text)) {
                out.push((*id, search::snippet(&text, &terms, 60).unwrap_or_default()));
            }
        }
        out
    }

    // ---- watched folders -----------------------------------------------------------------------

    /// PDFs in the watched folders that aren't in the library yet.
    pub fn scan_watched(&self) -> Vec<String> {
        let mut found = Vec::new();
        for folder in &self.settings.watched {
            let mut budget = MAX_SCAN_ENTRIES;
            collect_pdfs(Path::new(folder), 0, &mut budget, &mut found);
        }
        found.retain(|p| self.find_path(p).is_none());
        found.sort();
        found.dedup();
        found
    }
}

fn collect_pdfs(dir: &Path, depth: usize, budget: &mut usize, out: &mut Vec<String>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        if kind.is_dir() && !hidden {
            collect_pdfs(&path, depth + 1, budget, out);
        } else if kind.is_file() && !hidden && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
            out.push(path.to_string_lossy().into_owned());
        }
    }
}

fn canonical(path: &str) -> Option<String> {
    std::fs::canonicalize(path).ok().map(|p| p.to_string_lossy().into_owned())
}

/// Write `bytes` to `path` through a temporary file in the same folder.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Scope;

    fn tempdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pdfcraft-library-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trip_and_ids_never_reused() {
        let d = tempdir("roundtrip");
        let pdf = d.join("a.pdf");
        std::fs::write(&pdf, b"%PDF-1.4").unwrap();
        let (mut lib, err) = Library::open(&d);
        assert!(err.is_none() && lib.is_empty());
        let (id, new) = lib.add(pdf.to_str().unwrap());
        assert!(new);
        assert_eq!(lib.add(pdf.to_str().unwrap()), (id, false), "same file isn't added twice");
        lib.get_mut(id).unwrap().notes = "my note".into();
        lib.touch(id);
        lib.add_tag(id, "#Psychometrics");
        lib.add_tag(id, "psychometrics");
        let c = lib.add_collection("Thesis", None);
        lib.set_in_collection(id, c, true);
        lib.save().unwrap();
        assert!(!lib.is_dirty());

        let (mut again, err) = Library::open(&d);
        assert!(err.is_none());
        let p = again.get(id).unwrap();
        assert_eq!(p.notes, "my note");
        assert_eq!(p.tags, vec!["Psychometrics"]);
        assert_eq!(again.collection_count(c), 1);
        again.remove(id);
        let (id2, _) = again.add(pdf.to_str().unwrap());
        assert!(id2.0 > c.0, "ids are never reused");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn damaged_file_is_kept_aside() {
        let d = tempdir("damaged");
        std::fs::write(d.join("library.json"), b"{ not json").unwrap();
        let (lib, err) = Library::open(&d);
        assert!(lib.is_empty());
        assert!(matches!(err, Some(Error::Format(_))));
        assert!(std::fs::read_dir(&d).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains("damaged")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn collections_tags_listing_fulltext_and_watch() {
        let d = tempdir("misc");
        let watched = d.join("papers");
        std::fs::create_dir_all(watched.join("sub")).unwrap();
        std::fs::write(watched.join("one.pdf"), b"").unwrap();
        std::fs::write(watched.join("sub").join("TWO.PDF"), b"").unwrap();
        std::fs::write(watched.join("notes.txt"), b"").unwrap();
        let mut lib = Library::empty(&d);
        lib.settings.watched.push(watched.to_string_lossy().into_owned());
        let new = lib.scan_watched();
        assert_eq!(new.len(), 2);
        let ids: Vec<PaperId> = new.iter().map(|p| lib.add(p).0).collect();
        assert!(lib.scan_watched().is_empty());

        let parent = lib.add_collection("", None);
        let child = lib.add_collection("Child", Some(parent));
        lib.set_in_collection(ids[0], child, true);
        lib.remove_collection(parent);
        assert!(lib.collections.is_empty());
        assert!(lib.get(ids[0]).unwrap().collections.is_empty());

        lib.add_tag(ids[0], "irt");
        lib.add_tag(ids[1], "IRT");
        assert_eq!(lib.tags(), vec![("irt".to_owned(), 2)]);

        lib.set_fulltext(ids[1], "The Rasch model fits.").unwrap();
        let hits = Library::fulltext_hits(lib.dir(), &ids, "rasch FITS");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, ids[1]);
        let filter = Filter { scope: Scope::All, query: "rasch".into() };
        assert!(lib.list(&filter, SortKey::Added, true, &[]).is_empty());
        assert_eq!(lib.list(&filter, SortKey::Added, true, &[ids[1]]).len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
