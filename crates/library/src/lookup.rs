//! Turning CrossRef and arXiv answers into [`Metadata`].
//!
//! Only parsing lives here (always built, tested on fixtures); the requests themselves are in
//! `fetch` behind the `fetch` feature. Answers are untrusted: every field is optional, strings
//! are cleaned of markup and capped.

use serde_json::Value;

use crate::model::{Author, Metadata};
use crate::{Error, Result};

/// The longest abstract kept.
const MAX_ABSTRACT: usize = 20_000;
/// The most authors kept from one answer.
const MAX_AUTHORS: usize = 500;

/// The CrossRef REST URL for a DOI (`https://api.crossref.org/works/{doi}`).
pub fn crossref_url(doi: &str) -> String {
    format!("https://api.crossref.org/works/{}", encode_path(doi.trim()))
}

/// The arXiv API URL for an id.
pub fn arxiv_url(id: &str) -> String {
    format!("https://export.arxiv.org/api/query?id_list={}", encode_path(id.trim()))
}

/// Percent-encode everything but unreserved characters and `/` (DOIs keep their slash).
fn encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A CrossRef `/works/{doi}` answer.
pub fn parse_crossref(body: &str) -> Result<Metadata> {
    let root: Value = serde_json::from_str(body).map_err(|e| Error::Lookup(format!("CrossRef answered with something unreadable ({e})")))?;
    let m = root.get("message").unwrap_or(&root);
    let first = |key: &str| -> String {
        match m.get(key) {
            Some(Value::Array(a)) => a.iter().find_map(Value::as_str).map(clean).unwrap_or_default(),
            Some(Value::String(s)) => clean(s),
            _ => String::new(),
        }
    };
    let text = |key: &str| m.get(key).and_then(Value::as_str).map(clean).unwrap_or_default();
    let authors = m
        .get("author")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(MAX_AUTHORS)
                .filter_map(|p| {
                    let given = p.get("given").and_then(Value::as_str).unwrap_or("");
                    let family = p.get("family").and_then(Value::as_str).or_else(|| p.get("name").and_then(Value::as_str)).unwrap_or("");
                    (!given.is_empty() || !family.is_empty()).then(|| Author::new(&clean(given), &clean(family)))
                })
                .collect()
        })
        .unwrap_or_default();
    let year = ["published-print", "published-online", "issued", "created"]
        .iter()
        .find_map(|k| m.get(*k)?.get("date-parts")?.get(0)?.get(0)?.as_i64())
        .and_then(|y| i32::try_from(y).ok())
        .filter(|y| (1000..=3000).contains(y));
    let mut abstract_text = strip_tags(&text("abstract"));
    cap(&mut abstract_text, MAX_ABSTRACT);
    let keywords =
        m.get("subject").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(clean).take(50).collect()).unwrap_or_default();
    Ok(Metadata {
        kind: crossref_kind(&text("type")).to_owned(),
        title: first("title"),
        authors,
        year,
        venue: first("container-title"),
        volume: text("volume"),
        issue: text("issue"),
        pages: text("page").replace('-', "–"),
        publisher: text("publisher"),
        doi: text("DOI").to_lowercase(),
        isbn: first("ISBN").chars().filter(|c| c.is_ascii_digit() || *c == 'X').collect(),
        url: text("URL"),
        abstract_text,
        keywords,
        ..Default::default()
    })
}

/// CrossRef work types → BibTeX entry types.
fn crossref_kind(t: &str) -> &'static str {
    match t {
        "journal-article" => "article",
        "proceedings-article" => "inproceedings",
        "book-chapter" | "book-section" | "book-part" => "incollection",
        "book" | "monograph" | "edited-book" | "reference-book" => "book",
        "dissertation" => "phdthesis",
        "report" => "techreport",
        "posted-content" => "misc",
        _ => "article",
    }
}

/// An arXiv API Atom feed with one entry.
pub fn parse_arxiv(body: &str) -> Result<Metadata> {
    let doc = roxmltree::Document::parse(body).map_err(|e| Error::Lookup(format!("arXiv answered with something unreadable ({e})")))?;
    let entry = doc.descendants().find(|n| n.has_tag_name("entry")).ok_or_else(|| Error::Lookup("arXiv has no paper with that id".into()))?;
    let child_text = |name: &str| entry.children().find(|n| n.has_tag_name(name)).and_then(|n| n.text()).map(clean).unwrap_or_default();
    let title = child_text("title");
    // arXiv reports a missing id as an entry titled "Error".
    if title.eq_ignore_ascii_case("error") {
        return Err(Error::Lookup("arXiv has no paper with that id".into()));
    }
    let authors = entry
        .children()
        .filter(|n| n.has_tag_name("author"))
        .take(MAX_AUTHORS)
        .filter_map(|a| a.children().find(|n| n.has_tag_name("name")).and_then(|n| n.text()))
        .filter_map(|n| Author::parse(&clean(n)))
        .collect();
    let year = child_text("published").get(..4).and_then(|y| y.parse::<i32>().ok());
    let id_url = child_text("id");
    let arxiv = id_url.rsplit("/abs/").next().map(strip_version).unwrap_or_default();
    let doi = entry.children().find(|n| n.has_tag_name("doi")).and_then(|n| n.text()).map(|d| clean(d).to_lowercase()).unwrap_or_default();
    let venue = entry.children().find(|n| n.has_tag_name("journal_ref")).and_then(|n| n.text()).map(clean).unwrap_or_default();
    let keywords = entry.children().filter(|n| n.has_tag_name("category")).filter_map(|n| n.attribute("term")).map(str::to_owned).take(20).collect();
    let mut abstract_text = child_text("summary");
    cap(&mut abstract_text, MAX_ABSTRACT);
    Ok(Metadata {
        kind: if venue.is_empty() { "misc".into() } else { "article".into() },
        title,
        authors,
        year,
        venue,
        publisher: "arXiv".into(),
        doi,
        arxiv: arxiv.clone(),
        url: if arxiv.is_empty() { String::new() } else { format!("https://arxiv.org/abs/{arxiv}") },
        abstract_text,
        keywords,
        ..Default::default()
    })
}

fn strip_version(id: &str) -> String {
    match id.rfind('v') {
        Some(i) if i > 0 && id.get(i + 1..).is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit())) => {
            id.get(..i).unwrap_or(id).to_owned()
        }
        _ => id.to_owned(),
    }
}

/// Collapse runs of whitespace (titles arrive with line breaks) and trim.
pub fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Remove JATS/HTML tags (`<jats:p>`) from an abstract and decode the common entities.
pub fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&");
    let out = clean(&out);
    out.strip_prefix("Abstract ").map(str::to_owned).unwrap_or(out)
}

/// Cut `s` to at most `max` bytes at a char boundary.
fn cap(s: &mut String, max: usize) {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CROSSREF: &str = r#"{"status":"ok","message":{
        "DOI":"10.1038/NATURE14539","type":"journal-article","title":["Deep\n learning"],
        "author":[{"given":"Yann","family":"LeCun"},{"given":"Yoshua","family":"Bengio"},{"name":"The Consortium"}],
        "container-title":["Nature"],"volume":"521","issue":"7553","page":"436-444","publisher":"Springer",
        "published-print":{"date-parts":[[2015,5,28]]},
        "abstract":"<jats:title>Abstract</jats:title><jats:p>Deep learning allows &amp; enables.</jats:p>",
        "subject":["Multidisciplinary"],"URL":"http://dx.doi.org/10.1038/nature14539"}}"#;

    #[test]
    fn crossref() {
        let m = parse_crossref(CROSSREF).unwrap();
        assert_eq!(m.title, "Deep learning");
        assert_eq!(m.kind, "article");
        assert_eq!(m.authors.len(), 3);
        assert_eq!(m.authors[2], Author::new("", "The Consortium"));
        assert_eq!(m.year, Some(2015));
        assert_eq!((m.venue.as_str(), m.volume.as_str(), m.issue.as_str(), m.pages.as_str()), ("Nature", "521", "7553", "436–444"));
        assert_eq!(m.doi, "10.1038/nature14539");
        assert_eq!(m.abstract_text, "Deep learning allows & enables.");
        assert!(parse_crossref("not json").is_err());
        assert_eq!(parse_crossref("{}").unwrap(), Metadata { kind: "article".into(), ..Default::default() });
    }

    const ARXIV: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/1706.03762v7</id>
    <published>2017-06-12T17:57:34Z</published>
    <title>Attention Is All
      You Need</title>
    <summary>  The dominant sequence transduction models...  </summary>
    <author><name>Ashish Vaswani</name></author>
    <author><name>Noam Shazeer</name></author>
    <arxiv:doi>10.48550/arXiv.1706.03762</arxiv:doi>
    <category term="cs.CL"/><category term="cs.LG"/>
  </entry>
</feed>"#;

    #[test]
    fn arxiv() {
        let m = parse_arxiv(ARXIV).unwrap();
        assert_eq!(m.title, "Attention Is All You Need");
        assert_eq!(m.arxiv, "1706.03762");
        assert_eq!(m.year, Some(2017));
        assert_eq!(m.authors[1], Author::new("Noam", "Shazeer"));
        assert_eq!(m.doi, "10.48550/arxiv.1706.03762");
        assert_eq!(m.keywords, vec!["cs.CL", "cs.LG"]);
        assert_eq!(m.abstract_text, "The dominant sequence transduction models...");
        assert!(parse_arxiv("<feed xmlns=\"http://www.w3.org/2005/Atom\"></feed>").is_err());
        assert!(parse_arxiv("<feed").is_err());
    }

    #[test]
    fn urls_and_helpers() {
        assert_eq!(crossref_url("10.1002/(SICI)1097"), "https://api.crossref.org/works/10.1002/%28SICI%291097");
        assert_eq!(strip_version("1706.03762v7"), "1706.03762");
        assert_eq!(strip_version("hep-th/9901001"), "hep-th/9901001");
        let mut s = "ééé".to_owned();
        cap(&mut s, 3);
        assert_eq!(s, "é");
    }
}
