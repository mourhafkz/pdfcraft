//! The records a library keeps.

use serde::{Deserialize, Serialize};

/// A paper's id within its library; never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PaperId(pub u64);

/// A collection's id within its library; never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CollectionId(pub u64);

/// One author. `family` is the surname; `given` may be initials or empty (an organisation).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub given: String,
    #[serde(default)]
    pub family: String,
}

impl Author {
    pub fn new(given: &str, family: &str) -> Self {
        Author { given: given.trim().to_owned(), family: family.trim().to_owned() }
    }

    /// Parse one name as people type it: "Family, Given" or "Given Family". A single word is a
    /// surname.
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        if let Some((family, given)) = name.split_once(',') {
            return Some(Author::new(given, family));
        }
        match name.rsplit_once(char::is_whitespace) {
            Some((given, family)) => Some(Author::new(given, family)),
            None => Some(Author::new("", name)),
        }
    }

    /// Parse an author list separated by `;` or by " and " (BibTeX style). A list with neither
    /// but with commas is read as "Given Family, Given Family" when no part looks like a lone
    /// given name.
    pub fn parse_list(list: &str) -> Vec<Self> {
        let list = list.trim();
        if list.is_empty() {
            return Vec::new();
        }
        let parts: Vec<&str> = if list.contains(';') {
            list.split(';').collect()
        } else if list.contains(" and ") {
            list.split(" and ").collect()
        } else if list.matches(',').count() >= 2 || (list.contains(',') && list.split(',').all(|p| p.trim().contains(' '))) {
            // "Ada Lovelace, Alan Turing, Grace Hopper"
            return list.split(',').filter_map(|p| Author::parse(p.trim().trim_end_matches(','))).collect();
        } else {
            vec![list]
        };
        parts.into_iter().filter_map(Author::parse).collect()
    }

    /// "Family, G." for reference lists.
    pub fn family_initials(&self) -> String {
        let initials = initials(&self.given);
        match (self.family.is_empty(), initials.is_empty()) {
            (true, _) => self.given.clone(),
            (false, true) => self.family.clone(),
            (false, false) => format!("{}, {}", self.family, initials),
        }
    }

    /// "Given Family" for display.
    pub fn display(&self) -> String {
        match (self.given.is_empty(), self.family.is_empty()) {
            (true, _) => self.family.clone(),
            (false, true) => self.given.clone(),
            (false, false) => format!("{} {}", self.given, self.family),
        }
    }
}

/// "Ada Byron" → "A. B."; hyphenated names keep the hyphen ("Jean-Paul" → "J.-P.").
pub fn initials(given: &str) -> String {
    let mut out = Vec::new();
    for word in given.split_whitespace() {
        let parts: Vec<String> =
            word.split('-').filter_map(|p| p.chars().find(|c| c.is_alphabetic())).map(|c| format!("{}.", c.to_uppercase())).collect();
        if !parts.is_empty() {
            out.push(parts.join("-"));
        }
    }
    out.join(" ")
}

/// How far the user has got with a paper.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadState {
    #[default]
    Unread,
    Reading,
    Read,
}

impl ReadState {
    pub const ALL: [ReadState; 3] = [ReadState::Unread, ReadState::Reading, ReadState::Read];

    pub fn label(self) -> &'static str {
        match self {
            ReadState::Unread => "Unread",
            ReadState::Reading => "Reading",
            ReadState::Read => "Read",
        }
    }
}

/// Bibliographic facts about a paper, as edited by the user or found by a lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    /// BibTeX entry type: article, inproceedings, book, incollection, phdthesis, misc, ...
    pub kind: String,
    pub title: String,
    pub authors: Vec<Author>,
    pub year: Option<i32>,
    /// Journal, proceedings or publisher series.
    pub venue: String,
    pub volume: String,
    pub issue: String,
    pub pages: String,
    pub publisher: String,
    pub doi: String,
    pub arxiv: String,
    pub isbn: String,
    pub url: String,
    pub abstract_text: String,
    pub keywords: Vec<String>,
}

impl Metadata {
    /// Fill every empty field of `self` from `other`; fields the user already filled stay.
    pub fn fill_from(&mut self, other: &Metadata) {
        fn take(dst: &mut String, src: &str) {
            if dst.trim().is_empty() && !src.trim().is_empty() {
                src.trim().clone_into(dst);
            }
        }
        take(&mut self.kind, &other.kind);
        take(&mut self.title, &other.title);
        if self.authors.is_empty() {
            self.authors = other.authors.clone();
        }
        if self.year.is_none() {
            self.year = other.year;
        }
        take(&mut self.venue, &other.venue);
        take(&mut self.volume, &other.volume);
        take(&mut self.issue, &other.issue);
        take(&mut self.pages, &other.pages);
        take(&mut self.publisher, &other.publisher);
        take(&mut self.doi, &other.doi);
        take(&mut self.arxiv, &other.arxiv);
        take(&mut self.isbn, &other.isbn);
        take(&mut self.url, &other.url);
        take(&mut self.abstract_text, &other.abstract_text);
        if self.keywords.is_empty() {
            self.keywords = other.keywords.clone();
        }
    }

    /// Replace every field `other` has a value for (a lookup the user asked to overwrite with).
    pub fn overwrite_from(&mut self, other: &Metadata) {
        let mut merged = other.clone();
        merged.fill_from(self);
        *self = merged;
    }

    /// "Lovelace", "Lovelace & Turing" or "Lovelace et al." for tables.
    pub fn author_short(&self) -> String {
        let family = |a: &Author| if a.family.is_empty() { a.given.clone() } else { a.family.clone() };
        match self.authors.as_slice() {
            [] => String::new(),
            [a] => family(a),
            [a, b] => format!("{} & {}", family(a), family(b)),
            [a, ..] => format!("{} et al.", family(a)),
        }
    }

    /// All authors as "Given Family; Given Family" for editing.
    pub fn authors_text(&self) -> String {
        self.authors
            .iter()
            .map(|a| if a.given.is_empty() { a.family.clone() } else { format!("{}, {}", a.family, a.given) })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// One PDF in the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Paper {
    pub id: PaperId,
    /// The PDF on disk (absolute). A paper whose file went missing keeps its record.
    pub path: String,
    /// The file's size when it was added, a cheap duplicate check alongside the path.
    #[serde(default)]
    pub file_size: u64,
    #[serde(default)]
    pub meta: Metadata,
    /// User tags (lower-case kept as typed; matched case-insensitively).
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub collections: Vec<CollectionId>,
    /// Free-form notes (Markdown).
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub read_state: ReadState,
    #[serde(default)]
    pub favorite: bool,
    /// RFC 3339 times.
    #[serde(default)]
    pub added: String,
    #[serde(default)]
    pub modified: String,
    #[serde(default)]
    pub last_opened: String,
    /// BibTeX citation key the user fixed; empty means "generate one".
    #[serde(default)]
    pub cite_key: String,
    /// Whether a metadata lookup has already been tried, so automatic lookups don't repeat.
    #[serde(default)]
    pub lookup_tried: bool,
}

impl Paper {
    /// The title, or the file name when the title is still unknown.
    pub fn display_title(&self) -> String {
        if !self.meta.title.trim().is_empty() {
            return self.meta.title.trim().to_owned();
        }
        std::path::Path::new(&self.path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| self.path.clone())
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    }
}

/// A user folder of papers (Mendeley's collections). Flat; `parent` allows one level of nesting
/// in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: CollectionId,
    pub name: String,
    #[serde(default)]
    pub parent: Option<CollectionId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_both_ways() {
        assert_eq!(Author::parse("Lovelace, Ada"), Some(Author::new("Ada", "Lovelace")));
        assert_eq!(Author::parse("Ada King Lovelace"), Some(Author::new("Ada King", "Lovelace")));
        assert_eq!(Author::parse("UNESCO"), Some(Author::new("", "UNESCO")));
        assert_eq!(Author::parse("  "), None);
    }

    #[test]
    fn parses_lists() {
        assert_eq!(Author::parse_list("Lovelace, Ada; Turing, Alan").len(), 2);
        assert_eq!(Author::parse_list("Ada Lovelace and Alan Turing")[1], Author::new("Alan", "Turing"));
        assert_eq!(Author::parse_list("Ada Lovelace, Alan Turing, Grace Hopper").len(), 3);
        assert_eq!(Author::parse_list("Lovelace, Ada"), vec![Author::new("Ada", "Lovelace")]);
        assert!(Author::parse_list("").is_empty());
    }

    #[test]
    fn initials_and_short_forms() {
        assert_eq!(initials("Jean-Paul Marie"), "J.-P. M.");
        assert_eq!(Author::new("Ada", "Lovelace").family_initials(), "Lovelace, A.");
        let mut m = Metadata::default();
        assert_eq!(m.author_short(), "");
        m.authors = vec![Author::new("A", "One"), Author::new("B", "Two"), Author::new("C", "Three")];
        assert_eq!(m.author_short(), "One et al.");
        assert_eq!(m.authors_text(), "One, A; Two, B; Three, C");
        assert_eq!(Author::parse_list(&m.authors_text()), m.authors);
    }

    #[test]
    fn fill_keeps_user_values_and_overwrite_replaces() {
        let mut mine = Metadata { title: "Mine".into(), ..Default::default() };
        let found = Metadata { title: "Found".into(), year: Some(2020), doi: "10.1/x".into(), ..Default::default() };
        mine.fill_from(&found);
        assert_eq!((mine.title.as_str(), mine.year, mine.doi.as_str()), ("Mine", Some(2020), "10.1/x"));
        mine.overwrite_from(&Metadata { title: "New".into(), ..Default::default() });
        assert_eq!((mine.title.as_str(), mine.year), ("New", Some(2020)));
    }
}
