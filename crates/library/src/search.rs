//! Which papers the library view shows, in what order.

use crate::model::{CollectionId, Paper, ReadState};

/// The sidebar choice plus the search box.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub scope: Scope,
    /// Words that must all appear (case-insensitively) in the title, authors, venue, year, DOI,
    /// abstract, keywords, tags or notes.
    pub query: String,
}

/// What the sidebar selected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum Scope {
    #[default]
    All,
    Recent,
    Favorites,
    State(ReadState),
    /// Papers in no collection.
    Unfiled,
    Collection(CollectionId),
    Tag(String),
    /// Papers whose metadata still lacks a title or authors.
    NeedsReview,
}

/// Columns the table sorts by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SortKey {
    #[default]
    Added,
    Title,
    Authors,
    Year,
    Venue,
    LastOpened,
}

/// Lower-cased with Turkish dotted/dotless i folded, as the document search does.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'I' | 'i' | '\u{130}' | '\u{131}' => out.push('i'),
            _ => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// The searchable text of a paper's record (not its full text).
pub fn haystack(p: &Paper) -> String {
    let m = &p.meta;
    let mut s = String::new();
    for part in [
        m.title.as_str(),
        m.venue.as_str(),
        m.doi.as_str(),
        m.arxiv.as_str(),
        m.abstract_text.as_str(),
        m.publisher.as_str(),
        p.notes.as_str(),
        p.path.as_str(),
    ] {
        s.push_str(part);
        s.push('\n');
    }
    for a in &m.authors {
        s.push_str(&a.given);
        s.push(' ');
        s.push_str(&a.family);
        s.push('\n');
    }
    if let Some(y) = m.year {
        s.push_str(&y.to_string());
        s.push('\n');
    }
    for k in m.keywords.iter().chain(p.tags.iter()) {
        s.push_str(k);
        s.push('\n');
    }
    fold(&s)
}

/// The query's words, folded; empty words dropped.
pub fn terms(query: &str) -> Vec<String> {
    fold(query).split_whitespace().map(str::to_owned).collect()
}

/// Whether every term appears in `folded_text`.
pub fn all_terms_in(terms: &[String], folded_text: &str) -> bool {
    terms.iter().all(|t| folded_text.contains(t.as_str()))
}

impl Scope {
    pub fn admits(&self, p: &Paper, recent_since: &str) -> bool {
        match self {
            Scope::All => true,
            Scope::Recent => p.added.as_str() >= recent_since,
            Scope::Favorites => p.favorite,
            Scope::State(s) => p.read_state == *s,
            Scope::Unfiled => p.collections.is_empty(),
            Scope::Collection(c) => p.collections.contains(c),
            Scope::Tag(t) => p.has_tag(t),
            Scope::NeedsReview => p.meta.title.trim().is_empty() || p.meta.authors.is_empty() || p.meta.year.is_none(),
        }
    }
}

/// Sort `papers` by `key`; `ascending` flips the natural order. Ties fall back to the id, so the
/// order is stable across frames.
pub fn sort(papers: &mut [&Paper], key: SortKey, ascending: bool) {
    papers.sort_by(|a, b| {
        let ord = match key {
            SortKey::Added => a.added.cmp(&b.added),
            SortKey::Title => fold(&a.display_title()).cmp(&fold(&b.display_title())),
            SortKey::Authors => fold(&a.meta.author_short()).cmp(&fold(&b.meta.author_short())),
            SortKey::Year => a.meta.year.cmp(&b.meta.year),
            SortKey::Venue => fold(&a.meta.venue).cmp(&fold(&b.meta.venue)),
            SortKey::LastOpened => a.last_opened.cmp(&b.last_opened),
        };
        let ord = ord.then(a.id.cmp(&b.id));
        if ascending { ord } else { ord.reverse() }
    });
}

/// A short excerpt of `text` around the first term found, for search results (char-safe).
pub fn snippet(text: &str, terms: &[String], radius: usize) -> Option<String> {
    let folded = fold(text);
    // Folding can change byte lengths, so map through char positions.
    let (first, len) = terms
        .iter()
        .filter_map(|t| folded.find(t.as_str()).map(|b| (folded.get(..b).map(|s| s.chars().count()).unwrap_or(0), t.chars().count())))
        .min()?;
    let chars: Vec<char> = text.chars().collect();
    let start = first.saturating_sub(radius);
    let end = first.saturating_add(len).saturating_add(radius).min(chars.len());
    let body: String = chars.get(start..end)?.iter().collect();
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(format!("{}{}{}", if start > 0 { "…" } else { "" }, body, if end < chars.len() { "…" } else { "" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Author, Metadata, PaperId};

    fn p(id: u64, title: &str, year: i32) -> Paper {
        Paper {
            id: PaperId(id),
            path: format!("/x/{id}.pdf"),
            file_size: 0,
            meta: Metadata { title: title.into(), year: Some(year), authors: vec![Author::new("Ada", "Lovelace")], ..Default::default() },
            tags: vec!["IRT".into()],
            collections: vec![],
            notes: String::new(),
            read_state: ReadState::Unread,
            favorite: false,
            added: format!("2026-01-0{id}"),
            modified: String::new(),
            last_opened: String::new(),
            cite_key: String::new(),
            lookup_tried: false,
        }
    }

    #[test]
    fn matching_and_scopes() {
        let a = p(1, "Item Response Theory", 2001);
        let t = terms("  response   LOVELACE ");
        assert!(all_terms_in(&t, &haystack(&a)));
        assert!(!all_terms_in(&terms("turing"), &haystack(&a)));
        assert!(Scope::Tag("irt".into()).admits(&a, ""));
        assert!(Scope::Unfiled.admits(&a, ""));
        assert!(Scope::Recent.admits(&a, "2026-01-01"));
        assert!(!Scope::Recent.admits(&a, "2026-02-01"));
        assert!(!Scope::NeedsReview.admits(&a, ""));
        assert_eq!(fold("İSTANBUL"), "istanbul");
    }

    #[test]
    fn sorting_is_stable_and_reversible() {
        let (a, b, c) = (p(1, "beta", 2000), p(2, "Alpha", 2010), p(3, "alpha", 2005));
        let mut v = vec![&a, &b, &c];
        sort(&mut v, SortKey::Title, true);
        assert_eq!(v.iter().map(|p| p.id.0).collect::<Vec<_>>(), vec![2, 3, 1]);
        sort(&mut v, SortKey::Year, false);
        assert_eq!(v.iter().map(|p| p.id.0).collect::<Vec<_>>(), vec![2, 3, 1]);
    }

    #[test]
    fn snippets() {
        assert_eq!(snippet("aaaa ÉTÉ bbbb", &terms("été"), 2).as_deref(), Some("…a ÉTÉ b…"));
        assert_eq!(snippet("nothing", &terms("x"), 5), None);
        assert_eq!(snippet("", &[], 5), None);
    }
}
