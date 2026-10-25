//! Finding a paper's identifiers and a likely title in its text.
//!
//! The text comes from the PDF's first pages and is untrusted. Matching is done by hand on
//! chars (no regex engine), every slice is taken at a char boundary, and results are capped.

/// The longest DOI kept; real ones are far shorter.
const MAX_DOI: usize = 200;

/// The first DOI in `text`, lower-cased (DOIs are case-insensitive), without a trailing `.` or
/// `)` that belongs to the sentence. A DOI written as a URL (`https://doi.org/10...`) is found
/// too.
pub fn find_doi(text: &str) -> Option<String> {
    let mut from = 0;
    while let Some(off) = text.get(from..).and_then(|s| s.find("10.")) {
        let start = from + off;
        from = start + 3;
        // "10." must start a token: preceded by nothing, whitespace, '/', ':' or '('.
        let before = text.get(..start).and_then(|s| s.chars().next_back());
        if before.is_some_and(|c| !(c.is_whitespace() || matches!(c, '/' | ':' | '(' | '[' | '<' | '"'))) {
            continue;
        }
        let Some(rest) = text.get(start + 3..) else { continue };
        let prefix_len = rest.chars().take_while(char::is_ascii_digit).count();
        if !(4..=9).contains(&prefix_len) {
            continue;
        }
        let Some(after_prefix) = rest.get(prefix_len..) else { continue };
        let Some(suffix) = after_prefix.strip_prefix('/') else { continue };
        let mut end = 0;
        for (i, c) in suffix.char_indices() {
            if c.is_whitespace() || matches!(c, '"' | '<' | '>' | '{' | '}' | '|' | '\\' | '^' | '`') {
                break;
            }
            end = i + c.len_utf8();
        }
        let mut doi = format!("10.{}/{}", rest.get(..prefix_len).unwrap_or(""), suffix.get(..end).unwrap_or(""));
        trim_sentence_end(&mut doi);
        if doi.len() > MAX_DOI || doi.ends_with('/') {
            continue;
        }
        return Some(doi.to_lowercase());
    }
    None
}

/// Drop punctuation a sentence added after an identifier, and a `)` with no `(` to match.
fn trim_sentence_end(s: &mut String) {
    loop {
        let Some(last) = s.chars().next_back() else { return };
        let unmatched_close =
            (last == ')' && s.matches('(').count() < s.matches(')').count()) || (last == ']' && s.matches('[').count() < s.matches(']').count());
        if matches!(last, '.' | ',' | ';' | ':' | '\'') || unmatched_close {
            s.pop();
        } else {
            return;
        }
    }
}

/// The first arXiv id in `text`: new style (`2101.01234`, `2101.01234v2`) after "arXiv:" or in
/// an arxiv.org URL, or old style (`hep-th/9901001`). Returned without the "arXiv:" prefix and
/// without a version suffix.
pub fn find_arxiv(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for marker in ["arxiv:", "arxiv.org/abs/", "arxiv.org/pdf/"] {
        let mut from = 0;
        while let Some(off) = lower.get(from..).and_then(|s| s.find(marker)) {
            let start = from + off + marker.len();
            from = start;
            let Some(rest) = lower.get(start..) else { break };
            let rest = rest.trim_start();
            if let Some(id) = new_style(rest).or_else(|| old_style(rest)) {
                return Some(id);
            }
        }
    }
    None
}

/// `YYMM.NNNN` or `YYMM.NNNNN` at the start of `s`.
fn new_style(s: &str) -> Option<String> {
    let yymm: String = s.chars().take_while(char::is_ascii_digit).collect();
    if yymm.len() != 4 {
        return None;
    }
    let rest = s.get(4..)?.strip_prefix('.')?;
    let num: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if !(4..=5).contains(&num.len()) {
        return None;
    }
    Some(format!("{yymm}.{num}"))
}

/// `archive/YYMMNNN` (e.g. `hep-th/9901001`, `math.ag/0101001`) at the start of `s`.
fn old_style(s: &str) -> Option<String> {
    let archive: String = s.chars().take_while(|c| c.is_ascii_lowercase() || matches!(c, '-' | '.')).collect();
    if archive.len() < 2 {
        return None;
    }
    let rest = s.get(archive.len()..)?.strip_prefix('/')?;
    let num: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (num.len() == 7).then(|| format!("{archive}/{num}"))
}

/// The first ISBN-13 or ISBN-10 after the word "ISBN", digits only (X kept for ISBN-10), when
/// its check digit is right.
pub fn find_isbn(text: &str) -> Option<String> {
    let upper = text.to_uppercase();
    let mut from = 0;
    while let Some(off) = upper.get(from..).and_then(|s| s.find("ISBN")) {
        let start = from + off + 4;
        from = start;
        let Some(rest) = upper.get(start..) else { break };
        let digits: String = rest
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit() || matches!(c, '-' | ' ' | 'X'))
            .filter(|c| c.is_ascii_digit() || *c == 'X')
            .take(13)
            .collect();
        if digits.len() >= 13 && isbn13_ok(digits.get(..13).unwrap_or("")) {
            return digits.get(..13).map(str::to_owned);
        }
        if digits.len() >= 10 && isbn10_ok(digits.get(..10).unwrap_or("")) {
            return digits.get(..10).map(str::to_owned);
        }
    }
    None
}

fn isbn13_ok(d: &str) -> bool {
    let v: Vec<u32> = d.chars().filter_map(|c| c.to_digit(10)).collect();
    v.len() == 13 && v.iter().enumerate().map(|(i, x)| if i % 2 == 0 { *x } else { x * 3 }).sum::<u32>().is_multiple_of(10)
}

fn isbn10_ok(d: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in d.chars().enumerate() {
        let v = match c {
            'X' if i == 9 => 10,
            c => match c.to_digit(10) {
                Some(v) => v,
                None => return false,
            },
        };
        sum += v * (10 - i as u32);
    }
    d.chars().count() == 10 && sum.is_multiple_of(11)
}

/// Whether a PDF's Info `/Title` is a real title rather than a word processor's leftover
/// ("Microsoft Word - draft3.docx", "untitled", a file name).
pub fn plausible_info_title(title: &str) -> bool {
    let t = title.trim();
    let lower = t.to_lowercase();
    let words = t.split_whitespace().count();
    !(t.chars().count() < 6
        || words < 2
        || lower.starts_with("microsoft word")
        || lower.starts_with("untitled")
        || lower.contains(".doc")
        || lower.contains(".pdf")
        || lower.contains(".tex")
        || lower.contains(".dvi")
        || lower.starts_with("layout ")
        || lower.starts_with("pii:")
        || lower.starts_with("doi:"))
}

/// A likely title from the first page's text: the first line that reads like a heading (a few
/// words, no URL, no running header), joined with the line after it when that one continues it.
pub fn guess_title(first_page: &str) -> Option<String> {
    let lines: Vec<&str> = first_page.lines().map(str::trim).filter(|l| !l.is_empty()).take(40).collect();
    for (i, line) in lines.iter().enumerate() {
        if !title_like(line) {
            continue;
        }
        let mut title = (*line).to_owned();
        if let Some(next) = lines.get(i + 1)
            && title_like(next)
            && (title.ends_with(':') || title.ends_with('-') || next.chars().next().is_some_and(char::is_lowercase))
        {
            if title.ends_with('-') {
                title.pop();
            } else {
                title.push(' ');
            }
            title.push_str(next);
        }
        return Some(title);
    }
    None
}

/// Likely authors from the line under the title on the first page: two to four capitalised
/// words per name, names separated by commas, semicolons or "and". Footnote marks and digits
/// (affiliation numbers) are dropped.
pub fn guess_authors(first_page: &str, title: &str) -> Vec<crate::model::Author> {
    let lines: Vec<&str> = first_page.lines().map(str::trim).filter(|l| !l.is_empty()).take(40).collect();
    let title_end = title.split_whitespace().last().unwrap_or("");
    let Some(pos) =
        lines.iter().position(|l| !title_end.is_empty() && l.ends_with(title_end) && title.contains(l.split_whitespace().next().unwrap_or("\u{0}")))
    else {
        return Vec::new();
    };
    let Some(line) = lines.get(pos + 1) else { return Vec::new() };
    if line.chars().count() > 300 || line.contains('@') || line.to_lowercase().contains("university") {
        return Vec::new();
    }
    let cleaned: String = line.chars().filter(|c| !c.is_ascii_digit() && !matches!(c, '*' | '†' | '‡' | '§')).collect();
    let parts: Vec<&str> = cleaned.split([',', ';']).flat_map(|p| p.split(" and ")).map(str::trim).filter(|p| !p.is_empty()).collect();
    let name_like = |p: &&str| {
        let words: Vec<&str> = p.split_whitespace().collect();
        (2..=4).contains(&words.len()) && words.iter().all(|w| w.chars().next().is_some_and(char::is_uppercase))
    };
    if parts.is_empty() || parts.len() > 30 || !parts.iter().all(name_like) {
        return Vec::new();
    }
    parts.into_iter().filter_map(crate::model::Author::parse).collect()
}

fn title_like(line: &str) -> bool {
    let words = line.split_whitespace().count();
    let lower = line.to_lowercase();
    const SKIP: [&str; 16] = [
        "http",
        "www.",
        "doi",
        "journal",
        "vol.",
        "volume",
        "issn",
        "©",
        "copyright",
        "received",
        "accepted",
        "published",
        "proceedings",
        "preprint",
        "arxiv",
        "page ",
    ];
    (3..=25).contains(&words)
        && line.chars().count() <= 250
        && !SKIP.iter().any(|s| lower.contains(s))
        && !line.contains('@')
        && line.chars().filter(|c| c.is_alphabetic()).count() * 2 > line.chars().count()
}

/// Identifiers and a title guess found in a paper's text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub authors: Vec<crate::model::Author>,
    pub doi: Option<String>,
    pub arxiv: Option<String>,
    pub isbn: Option<String>,
    pub title: Option<String>,
}

/// Look through `first_pages` (the first page or two, joined) for identifiers, and through
/// `info_title` (the PDF's `/Title`) or the first page for a title.
pub fn scan(first_pages: &str, info_title: Option<&str>) -> Found {
    let first_page = first_pages.split('\u{c}').next().unwrap_or(first_pages);
    let title = info_title.filter(|t| plausible_info_title(t)).map(|t| t.trim().to_owned()).or_else(|| guess_title(first_page));
    Found {
        authors: title.as_deref().map(|t| guess_authors(first_page, t)).unwrap_or_default(),
        doi: find_doi(first_pages),
        arxiv: find_arxiv(first_pages),
        isbn: find_isbn(first_pages),
        title,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn finds_dois() {
        assert_eq!(find_doi("See doi:10.1038/nature14539."), Some("10.1038/nature14539".into()));
        assert_eq!(find_doi("https://doi.org/10.1016/J.Cell.2020.01.001 and more"), Some("10.1016/j.cell.2020.01.001".into()));
        assert_eq!(find_doi("(10.1093/mind/LIX.236.433)"), Some("10.1093/mind/lix.236.433".into()));
        assert_eq!(find_doi("doi 10.1002/(SICI)1097-4571(199806)49:8<693::AID-ASI4>3.0.CO;2-O"), Some("10.1002/(sici)1097-4571(199806)49:8".into()));
        assert_eq!(find_doi("version 10.12/x"), None, "prefix too short");
        assert_eq!(find_doi("x10.1038/nature"), None, "not at a token start");
        assert_eq!(find_doi("10.1038/"), None);
    }

    #[test]
    fn finds_arxiv_ids() {
        assert_eq!(find_arxiv("arXiv:2101.01234v2 [cs.CL] 3 Feb 2021"), Some("2101.01234".into()));
        assert_eq!(find_arxiv("https://arxiv.org/abs/1706.03762"), Some("1706.03762".into()));
        assert_eq!(find_arxiv("arXiv: hep-th/9901001"), Some("hep-th/9901001".into()));
        assert_eq!(find_arxiv("arXiv preprint"), None);
    }

    #[test]
    fn finds_isbns_with_valid_check_digits() {
        assert_eq!(find_isbn("ISBN 978-0-262-03384-8"), Some("9780262033848".into()));
        assert_eq!(find_isbn("ISBN: 0-306-40615-2"), Some("0306406152".into()));
        assert_eq!(find_isbn("ISBN 978-0-262-03384-9"), None);
    }

    #[test]
    fn titles() {
        assert!(!plausible_info_title("Microsoft Word - final.docx"));
        assert!(!plausible_info_title("paper.pdf"));
        assert!(plausible_info_title("Attention Is All You Need"));
        let page =
            "Journal of Testing, Vol. 3\nhttp://example.org\nAttention Is All You Need:\nA Study of Transformers\nAda Lovelace\nada@example.org";
        assert_eq!(guess_title(page), Some("Attention Is All You Need: A Study of Transformers".into()));
        assert_eq!(guess_title(""), None);
        let authors = guess_authors(page, "Attention Is All You Need: A Study of Transformers");
        assert_eq!(authors, vec![crate::model::Author::new("Ada", "Lovelace")]);
        let p2 = "Deep Nets\nGrace Hopper1, Alan M. Turing2* and Ada Lovelace\n1 University of X";
        assert_eq!(guess_authors(p2, "Deep Nets").len(), 3);
        assert!(guess_authors("Deep Nets\nAbstract. We show that this works.", "Deep Nets").is_empty());
        let found = scan("Some Title Of Paper\ndoi:10.1000/182\u{c}page two arXiv:2101.00001", Some("untitled"));
        assert_eq!(found.title.as_deref(), Some("Some Title Of Paper"));
        assert_eq!(found.doi.as_deref(), Some("10.1000/182"));
        assert_eq!(found.arxiv.as_deref(), Some("2101.00001"));
    }

    proptest! {
        #[test]
        fn never_panics(s in "\\PC{0,300}") {
            let _ = scan(&s, Some(&s));
        }

        #[test]
        fn never_panics_near_identifiers(s in "(10\\.[0-9]{0,10}/?[^ ]{0,8}|arxiv:|ISBN|[a-z\\-./()]){0,30}") {
            let _ = scan(&s, None);
        }
    }
}
