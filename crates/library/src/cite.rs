//! Citations and exports: BibTeX, RIS, APA 7 reference strings, and a Markdown digest of a
//! paper's notes and highlights.

use std::collections::HashSet;

use crate::model::{Author, Paper};

/// A citation key: family name of the first author, the year and the first meaningful word of
/// the title, ASCII only — `lecun2015deep`. Falls back to `paper<id>`.
pub fn generated_key(p: &Paper) -> String {
    if !p.cite_key.trim().is_empty() {
        return p.cite_key.trim().to_owned();
    }
    let ascii = |s: &str| -> String { s.chars().filter_map(fold_ascii).filter(char::is_ascii_alphanumeric).collect::<String>().to_lowercase() };
    let author = p.meta.authors.first().map(|a| ascii(if a.family.is_empty() { &a.given } else { &a.family })).unwrap_or_default();
    let year = p.meta.year.map(|y| y.to_string()).unwrap_or_default();
    const STOP: [&str; 12] = ["a", "an", "the", "on", "of", "in", "for", "to", "and", "with", "towards", "toward"];
    let word = p.meta.title.split_whitespace().map(ascii).find(|w| !w.is_empty() && !STOP.contains(&w.as_str())).unwrap_or_default();
    let key = format!("{author}{year}{word}");
    if key.is_empty() { format!("paper{}", p.id.0) } else { key }
}

/// Keys for several papers, made unique with a, b, c… suffixes in order.
pub fn unique_keys(papers: &[&Paper]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    papers
        .iter()
        .map(|p| {
            let base = generated_key(p);
            let mut key = base.clone();
            let mut n = 0u32;
            while !seen.insert(key.clone()) {
                n = n.saturating_add(1);
                key = format!("{base}{}", suffix(n));
            }
            key
        })
        .collect()
}

/// 1 → "a", 26 → "z", 27 → "aa".
fn suffix(mut n: u32) -> String {
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(char::from(b'a' + (n % 26) as u8));
        n /= 26;
    }
    s.iter().rev().collect()
}

/// A Latin letter with its accent dropped (é → e), for keys; other characters as they are.
fn fold_ascii(c: char) -> Option<char> {
    const FROM: &str = "àáâãäåāăąçćčďèéêëēėęěìíîïīįıłñńňòóôõöøōőřśšşťùúûüūůűųýÿźżžßÀÁÂÃÄÅĀĂĄÇĆČĎÈÉÊËĒĖĘĚÌÍÎÏĪĮİŁÑŃŇÒÓÔÕÖØŌŐŘŚŠŞŤÙÚÛÜŪŮŰŲÝŸŹŻŽ";
    const TO: &str = "aaaaaaaaacccdeeeeeeeeiiiiiiilnnnoooooooorssstuuuuuuuuyyzzzsAAAAAAAAACCCDEEEEEEEEIIIIIIILNNNOOOOOOOORSSSTUUUUUUUUYYZZZ";
    match FROM.chars().position(|f| f == c) {
        Some(i) => TO.chars().nth(i),
        None => Some(c),
    }
}

/// Escape BibTeX special characters in a field value (braces kept balanced).
fn bib_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' | '%' | '$' | '#' | '_' => {
                out.push('\\');
                out.push(c);
            }
            '{' | '}' | '\\' => {}
            '–' => out.push_str("--"),
            '—' => out.push_str("---"),
            _ => out.push(c),
        }
    }
    out
}

fn bib_author(a: &Author) -> String {
    match (a.given.is_empty(), a.family.is_empty()) {
        (true, _) => format!("{{{}}}", bib_escape(&a.family)),
        (false, true) => format!("{{{}}}", bib_escape(&a.given)),
        (false, false) => format!("{}, {}", bib_escape(&a.family), bib_escape(&a.given)),
    }
}

/// One BibTeX entry.
pub fn bibtex_entry(p: &Paper, key: &str) -> String {
    let m = &p.meta;
    let kind = if m.kind.trim().is_empty() { "article" } else { m.kind.trim() };
    let venue_field = match kind {
        "inproceedings" | "incollection" => "booktitle",
        "book" | "phdthesis" | "mastersthesis" | "techreport" | "misc" => "howpublished",
        _ => "journal",
    };
    let mut fields: Vec<(&str, String)> = Vec::new();
    let mut push = |name: &'static str, value: String| {
        if !value.trim().is_empty() {
            fields.push((name, value));
        }
    };
    push("title", format!("{{{}}}", bib_escape(&p.display_title())));
    push("author", m.authors.iter().map(bib_author).collect::<Vec<_>>().join(" and "));
    push("year", m.year.map(|y| y.to_string()).unwrap_or_default());
    push(venue_field, bib_escape(&m.venue));
    push("volume", bib_escape(&m.volume));
    push("number", bib_escape(&m.issue));
    push("pages", bib_escape(&m.pages));
    push("publisher", bib_escape(&m.publisher));
    push("doi", m.doi.clone());
    if !m.arxiv.is_empty() {
        push("eprint", m.arxiv.clone());
        push("archivePrefix", "arXiv".into());
    }
    push("isbn", m.isbn.clone());
    push("url", m.url.clone());
    push("abstract", bib_escape(&m.abstract_text));
    push("keywords", bib_escape(&p.meta.keywords.iter().chain(p.tags.iter()).cloned().collect::<Vec<_>>().join(", ")));
    push("file", bib_escape(&p.path));
    let body = fields.iter().map(|(k, v)| format!("  {k} = {{{v}}}")).collect::<Vec<_>>().join(",\n");
    format!("@{kind}{{{key},\n{body}\n}}\n")
}

/// A BibTeX file for `papers`.
pub fn bibtex(papers: &[&Paper]) -> String {
    let keys = unique_keys(papers);
    papers.iter().zip(keys).map(|(p, k)| bibtex_entry(p, &k)).collect::<Vec<_>>().join("\n")
}

/// One RIS record.
pub fn ris_entry(p: &Paper) -> String {
    let m = &p.meta;
    let ty = match m.kind.as_str() {
        "inproceedings" => "CPAPER",
        "incollection" => "CHAP",
        "book" => "BOOK",
        "phdthesis" | "mastersthesis" => "THES",
        "techreport" => "RPRT",
        "misc" => "GEN",
        _ => "JOUR",
    };
    let mut lines = vec![format!("TY  - {ty}")];
    let mut push = |tag: &str, value: &str| {
        let v = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !v.is_empty() {
            lines.push(format!("{tag}  - {v}"));
        }
    };
    push("TI", &p.display_title());
    for a in &m.authors {
        push("AU", &if a.given.is_empty() { a.family.clone() } else { format!("{}, {}", a.family, a.given) });
    }
    push("PY", &m.year.map(|y| y.to_string()).unwrap_or_default());
    push(if ty == "JOUR" { "JO" } else { "T2" }, &m.venue);
    push("VL", &m.volume);
    push("IS", &m.issue);
    let (sp, ep) = m.pages.split_once(['-', '–']).unwrap_or((m.pages.as_str(), ""));
    push("SP", sp);
    push("EP", ep);
    push("PB", &m.publisher);
    push("DO", &m.doi);
    push("SN", &m.isbn);
    push("UR", &m.url);
    push("AB", &m.abstract_text);
    for k in m.keywords.iter().chain(p.tags.iter()) {
        push("KW", k);
    }
    push("N1", &p.notes);
    push("L1", &p.path);
    lines.push("ER  - ".into());
    lines.join("\r\n") + "\r\n"
}

/// An RIS file for `papers`.
pub fn ris(papers: &[&Paper]) -> String {
    papers.iter().map(|p| ris_entry(p)).collect::<Vec<_>>().join("\r\n")
}

/// An APA 7 reference (plain text; the venue and volume would be italic in print).
pub fn apa(p: &Paper) -> String {
    let m = &p.meta;
    let names: Vec<String> = m.authors.iter().map(Author::family_initials).collect();
    let authors = match names.as_slice() {
        [] => String::new(),
        [a] => a.clone(),
        [init @ .., last] if names.len() <= 20 => format!("{}, & {}", init.join(", "), last),
        _ => {
            // APA 7: the first 19, an ellipsis, then the last.
            let first: Vec<&String> = names.iter().take(19).collect();
            format!("{}, … {}", first.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "), names.last().map(String::as_str).unwrap_or(""))
        }
    };
    let year = m.year.map(|y| y.to_string()).unwrap_or_else(|| "n.d.".into());
    let mut title = p.display_title();
    if !title.ends_with(['.', '?', '!']) {
        title.push('.');
    }
    let mut out = if authors.is_empty() { format!("{title} ({year}).") } else { format!("{authors} ({year}). {title}") };
    if !m.venue.is_empty() {
        out.push(' ');
        out.push_str(&m.venue);
        if !m.volume.is_empty() {
            out.push_str(&format!(", {}", m.volume));
            if !m.issue.is_empty() {
                out.push_str(&format!("({})", m.issue));
            }
        }
        if !m.pages.is_empty() {
            out.push_str(&format!(", {}", m.pages.replace('-', "–")));
        }
        out.push('.');
    } else if !m.publisher.is_empty() {
        out.push_str(&format!(" {}.", m.publisher));
    }
    if !m.doi.is_empty() {
        out.push_str(&format!(" https://doi.org/{}", m.doi));
    } else if !m.url.is_empty() {
        out.push_str(&format!(" {}", m.url));
    }
    out
}

/// An APA reference list for `papers`, sorted by first author and year as APA asks.
pub fn apa_list(papers: &[&Paper]) -> String {
    let mut refs: Vec<String> = papers.iter().map(|p| apa(p)).collect();
    refs.sort_by_key(|r| r.to_lowercase());
    refs.join("\n\n")
}

/// One highlight (or other markup comment) in a paper, as read from the PDF by the caller.
#[derive(Debug, Clone, PartialEq)]
pub struct HighlightNote {
    /// 0-based page.
    pub page: usize,
    /// "Highlight", "Underline", "StrikeOut", "Squiggly", "Text" (a sticky note), ...
    pub kind: String,
    /// The colour's name ("Yellow"), or empty.
    pub color: String,
    /// The text under the highlight, when known.
    pub text: String,
    /// The comment the user wrote on it.
    pub note: String,
}

/// A Markdown digest: the reference, the user's notes, then every highlight in page order,
/// grouped under its colour when `by_color`.
pub fn notes_markdown(p: &Paper, highlights: &[HighlightNote], by_color: bool) -> String {
    let mut out = format!("# {}\n\n{}\n", p.display_title(), apa(p));
    if !p.tags.is_empty() {
        out.push_str(&format!("\nTags: {}\n", p.tags.join(", ")));
    }
    if !p.notes.trim().is_empty() {
        out.push_str("\n## Notes\n\n");
        out.push_str(p.notes.trim());
        out.push('\n');
    }
    if highlights.is_empty() {
        return out;
    }
    out.push_str("\n## Highlights\n");
    let mut sorted: Vec<&HighlightNote> = highlights.iter().collect();
    sorted.sort_by_key(|h| h.page);
    let item = |h: &HighlightNote| -> String {
        let mut s = String::new();
        let quote = h.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !quote.is_empty() {
            s.push_str(&format!("\n> {quote} (p. {})\n", h.page.saturating_add(1)));
        } else {
            s.push_str(&format!("\n- {} on p. {}\n", h.kind, h.page.saturating_add(1)));
        }
        if !h.note.trim().is_empty() {
            s.push_str(&format!("\n{}\n", h.note.trim()));
        }
        s
    };
    if by_color {
        let mut colors: Vec<&str> = Vec::new();
        for h in &sorted {
            if !colors.contains(&h.color.as_str()) {
                colors.push(&h.color);
            }
        }
        for c in colors {
            out.push_str(&format!("\n### {}\n", if c.is_empty() { "Other" } else { c }));
            for h in sorted.iter().filter(|h| h.color == c) {
                out.push_str(&item(h));
            }
        }
    } else {
        for h in sorted {
            out.push_str(&item(h));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Metadata, PaperId};

    fn paper() -> Paper {
        Paper {
            id: PaperId(7),
            path: "/papers/lecun.pdf".into(),
            file_size: 0,
            meta: Metadata {
                kind: "article".into(),
                title: "Deep Learning".into(),
                authors: vec![Author::new("Yann", "LeCun"), Author::new("Yoshua", "Bengio"), Author::new("Geoffrey", "Hinton")],
                year: Some(2015),
                venue: "Nature".into(),
                volume: "521".into(),
                issue: "7553".into(),
                pages: "436-444".into(),
                doi: "10.1038/nature14539".into(),
                ..Default::default()
            },
            tags: vec!["dl".into()],
            collections: vec![],
            notes: "Seminal & short.".into(),
            read_state: Default::default(),
            favorite: false,
            added: String::new(),
            modified: String::new(),
            last_opened: String::new(),
            cite_key: String::new(),
            lookup_tried: false,
        }
    }

    #[test]
    fn keys() {
        let p = paper();
        assert_eq!(generated_key(&p), "lecun2015deep");
        let mut q = paper();
        q.meta.authors = vec![Author::new("Ánne", "Ñúñez")];
        q.meta.title = "The Ünique".into();
        assert_eq!(generated_key(&q), "nunez2015unique");
        let empty = Paper { meta: Metadata::default(), ..paper() };
        assert_eq!(generated_key(&empty), "paper7");
        assert_eq!(unique_keys(&[&p, &p, &p]), vec!["lecun2015deep", "lecun2015deepa", "lecun2015deepb"]);
        assert_eq!(suffix(27), "aa");
    }

    #[test]
    fn bibtex_output() {
        let b = bibtex(&[&paper()]);
        assert!(b.starts_with("@article{lecun2015deep,\n"));
        assert!(b.contains("  author = {LeCun, Yann and Bengio, Yoshua and Hinton, Geoffrey}"));
        assert!(b.contains("  journal = {Nature}"));
        assert!(b.contains("  title = {{Deep Learning}}"));
        assert_eq!(bib_escape("50% & {x}_1 – y"), "50\\% \\& x\\_1 -- y");
    }

    #[test]
    fn ris_output() {
        let r = ris(&[&paper()]);
        assert!(r.starts_with("TY  - JOUR\r\n"));
        assert!(r.contains("AU  - LeCun, Yann\r\n"));
        assert!(r.contains("SP  - 436\r\nEP  - 444\r\n"));
        assert!(r.ends_with("ER  - \r\n"));
    }

    #[test]
    fn apa_output() {
        assert_eq!(
            apa(&paper()),
            "LeCun, Y., Bengio, Y., & Hinton, G. (2015). Deep Learning. Nature, 521(7553), 436–444. https://doi.org/10.1038/nature14539"
        );
        let bare = Paper { meta: Metadata::default(), ..paper() };
        assert_eq!(apa(&bare), "lecun. (n.d.).");
    }

    #[test]
    fn markdown_digest() {
        let hs = vec![
            HighlightNote { page: 3, kind: "Highlight".into(), color: "Green".into(), text: "method   here".into(), note: String::new() },
            HighlightNote { page: 0, kind: "Highlight".into(), color: "Yellow".into(), text: "key finding".into(), note: "important".into() },
        ];
        let md = notes_markdown(&paper(), &hs, true);
        assert!(md.contains("## Notes\n\nSeminal & short."));
        let yellow = md.find("### Yellow").unwrap();
        let green = md.find("### Green").unwrap();
        assert!(yellow < green, "colours in page order of first use");
        assert!(md.contains("> key finding (p. 1)\n\nimportant\n"));
        assert!(md.contains("> method here (p. 4)"));
    }
}
