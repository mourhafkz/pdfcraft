//! Reading a paper for the library, off the interface thread: its identifiers and title (for
//! metadata), its text (for library-wide search), and its highlights with the text under them
//! and the notes written on them (the Library's Highlights tab, the Markdown digest).

use std::sync::Arc;

use pdfcraft_library::cite::HighlightNote;
use pdfcraft_library::ident::{self, Found};
use pdfcraft_render::{Annotation, DocInfo, PageInfo, PageRenderer, PageText, RenderConfig, RenderRequest, RequestKind};

/// Pages whose text is kept for search (long books are searched by their first pages).
const MAX_TEXT_PAGES: usize = 80;
/// The largest PDF read for the library.
const MAX_FILE: u64 = 512 << 20;

/// A highlight or other comment, ready to list.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub note: HighlightNote,
    /// The annotation's colour, 0..1.
    pub rgb: [f32; 3],
}

/// The comment kinds the Highlights tab lists.
pub fn listed(subtype: &str) -> bool {
    matches!(subtype, "Highlight" | "Underline" | "StrikeOut" | "Squiggly" | "Text" | "FreeText")
}

/// What reading a paper found.
#[derive(Debug, Default)]
pub struct Scanned {
    pub found: Found,
    /// From the PDF's Info dictionary.
    pub info_author: Option<String>,
    pub info_keywords: Option<String>,
    pub text: String,
    pub marks: Vec<Mark>,
}

pub fn read_file(path: &str) -> Result<Arc<Vec<u8>>, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_FILE {
        return Err("the file is too large to read for the library".into());
    }
    std::fs::read(path).map(Arc::new).map_err(|e| e.to_string())
}

/// Read everything the library wants from a PDF's bytes.
pub fn scan(bytes: Arc<Vec<u8>>, with_text: bool) -> Result<Scanned, String> {
    let info = pdfcraft_render::inspect(bytes.clone(), None).map_err(|e| e.to_string())?;
    let mut renderer = PageRenderer::new(bytes, RenderConfig::default());
    let mut texts: Vec<Option<Arc<PageText>>> = vec![None; info.pages.len()];
    let mut page_text = |page: usize| -> Option<Arc<PageText>> {
        let slot = texts.get_mut(page)?;
        if slot.is_none() {
            *slot = renderer.render(RenderRequest { page, kind: RequestKind::Text, scale: 1.0, ..Default::default() }).text;
        }
        slot.clone()
    };
    let mut text = String::new();
    let text_pages = if with_text { info.pages.len().min(MAX_TEXT_PAGES) } else { info.pages.len().min(2) };
    for page in 0..text_pages {
        if page > 0 {
            text.push('\u{c}');
        }
        if let Some(t) = page_text(page) {
            text.push_str(t.plain_text().trim_end());
        }
    }
    let first_two: String = text.split('\u{c}').take(2).collect::<Vec<_>>().join("\u{c}");
    let lines = page_text(0).map(|t| layout_lines(&t)).unwrap_or_default();
    let found = ident::scan_layout(&first_two, &lines, info.title.as_deref());
    let marks = marks(&info, &mut page_text);
    // Info /Author is often a word processor's account name ("anonymous", "admin", "jsmith"):
    // keep it only when every name in it appears on the first pages.
    let folded = pdfcraft_library::search::fold(&first_two);
    let info_author = info.author.clone().filter(|a| {
        let names = pdfcraft_library::model::Author::parse_list(a);
        !names.is_empty() && names.iter().all(|n| !n.family.is_empty() && folded.contains(&pdfcraft_library::search::fold(&n.family)))
    });
    Ok(Scanned {
        found,
        info_author,
        info_keywords: info.keywords.clone(),
        text: if with_text { text } else { String::new() },
        marks,
    })
}

/// A page's lines in reading order, each with its typical glyph height (its type size).
pub fn layout_lines(t: &PageText) -> Vec<ident::Line> {
    let mut lines: Vec<(String, Vec<f32>)> = Vec::new();
    let mut current: Option<u32> = None;
    for (i, g) in t.glyphs.iter().enumerate() {
        let line = t.line_of.get(i).copied().unwrap_or(0);
        if current != Some(line) || lines.is_empty() {
            lines.push((String::new(), Vec::new()));
            current = Some(line);
        } else if t.space_before.get(i).copied().unwrap_or(false)
            && let Some(last) = lines.last_mut()
        {
            last.0.push(' ');
        }
        if let Some(last) = lines.last_mut() {
            last.0.push_str(&g.text);
            let h = g.rect[3] - g.rect[1];
            if h.is_finite() && h > 0.0 && !g.text.trim().is_empty() {
                last.1.push(h);
            }
        }
        if lines.len() > 200 {
            break;
        }
    }
    lines
        .into_iter()
        .filter(|(text, _)| !text.trim().is_empty())
        .map(|(text, mut sizes)| {
            sizes.sort_by(f32::total_cmp);
            let size = sizes.get(sizes.len() / 2).copied().unwrap_or(0.0);
            ident::Line { text: text.trim().to_owned(), size }
        })
        .collect()
}

/// The listed comments of a document in page order, each with the text under it and the notes
/// written on it (its own contents, then its replies').
pub fn marks(info: &DocInfo, page_text: &mut dyn FnMut(usize) -> Option<Arc<PageText>>) -> Vec<Mark> {
    let anns = &info.annotations;
    let is_reply = |a: &Annotation| (a.in_reply_to.is_some() || a.parent.is_some()) && !a.grouped;
    let mut out = Vec::new();
    for a in anns.iter().filter(|a| listed(&a.subtype) && !is_reply(a) && !a.fill_sign) {
        let mut note = a.contents.clone().unwrap_or_default().trim().to_owned();
        for reply in anns.iter().filter(|r| is_reply(r) && r.parent == Some((a.page, a.index)) && r.state.is_none()) {
            let body = reply.contents.as_deref().unwrap_or("").trim();
            if !body.is_empty() {
                if !note.is_empty() {
                    note.push_str("\n\n");
                }
                note.push_str(body);
            }
        }
        let markup = matches!(a.subtype.as_str(), "Highlight" | "Underline" | "StrikeOut" | "Squiggly");
        let text = match (markup, info.pages.get(a.page)) {
            (true, Some(page)) => page_text(a.page).map(|t| text_under(&t, page, &a.quads)).unwrap_or_default(),
            _ => String::new(),
        };
        // Highlight comments often repeat the highlighted text as their contents; that is no note.
        if !text.is_empty() && squash(&note) == squash(&text) {
            note.clear();
        }
        let rgb = a.color.unwrap_or([1.0, 0.94, 0.0]);
        // Reading order: by page, then top to bottom (user space y grows upwards).
        let top = if a.rect[3].is_finite() { -a.rect[3] } else { 0.0 };
        out.push((
            a.page,
            top,
            Mark { note: HighlightNote { page: a.page, kind: a.subtype.clone(), color: color_name(rgb).to_owned(), text, note }, rgb },
        ));
    }
    out.sort_by(|x, y| x.0.cmp(&y.0).then(x.1.total_cmp(&y.1)));
    out.into_iter().map(|(_, _, m)| m).collect()
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The text of the glyphs whose centres fall inside the highlight's quadrilaterals.
pub fn text_under(t: &PageText, page: &PageInfo, quads: &[[f32; 8]]) -> String {
    let boxes: Vec<[f32; 4]> = quads
        .iter()
        .map(|q| {
            let pts = [page.user_to_view(q[0], q[1]), page.user_to_view(q[2], q[3]), page.user_to_view(q[4], q[5]), page.user_to_view(q[6], q[7])];
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for [x, y] in pts {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            [x0, y0, x1, y1]
        })
        .filter(|b| b.iter().all(|v| v.is_finite()) && b[0] < b[2] && b[1] < b[3])
        .collect();
    if boxes.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut prev: Option<usize> = None;
    for (i, g) in t.glyphs.iter().enumerate() {
        let (cx, cy) = ((g.rect[0] + g.rect[2]) * 0.5, (g.rect[1] + g.rect[3]) * 0.5);
        if !boxes.iter().any(|b| cx >= b[0] && cx <= b[2] && cy >= b[1] && cy <= b[3]) {
            continue;
        }
        if let Some(p) = prev {
            let new_line = t.line_of.get(i) != t.line_of.get(p);
            if new_line || i != p + 1 || t.space_before.get(i).copied().unwrap_or(false) {
                // A line break inside a highlight reads as a space; a hyphen at the line end joins.
                if new_line && out.ends_with('-') {
                    out.pop();
                } else {
                    out.push(' ');
                }
            }
        }
        out.push_str(&g.text);
        prev = Some(i);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The palette name closest to `rgb` (the comment swatches).
pub fn color_name(rgb: [f32; 3]) -> &'static str {
    crate::comments::SWATCHES
        .iter()
        .map(|(name, c)| {
            let d = (0..3).map(|k| (f64::from(rgb[k]) - c[k]).powi(2)).sum::<f64>();
            (name, d)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(n, _)| *n)
        .unwrap_or("Yellow")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_names() {
        assert_eq!(color_name([1.0, 0.95, 0.1]), "Yellow");
        assert_eq!(color_name([0.2, 0.6, 0.35]), "Green");
        assert_eq!(color_name([f32::NAN, 0.0, 0.0]), color_name([f32::NAN, 0.0, 0.0]));
    }

    #[test]
    fn listed_kinds() {
        assert!(listed("Highlight") && listed("Text") && !listed("Link") && !listed("Widget"));
    }
}
