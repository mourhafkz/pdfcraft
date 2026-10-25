//! Ask a paper: questions answered by Claude from the paper's own text, with every claim cited
//! to the page it came from.
//!
//! The paper goes to the Messages API as a *custom content document* with one content block per
//! page, so each citation's `start_block_index` is a page number. The reader's highlights and
//! notes, when included, go as a second document, so an answer can say "you marked this as a key
//! finding" and cite the reader's own note. Only the request is built and the answer parsed here;
//! the HTTPS call is in `fetch` behind the `fetch` feature.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Error, Result};

/// The model used unless the user picks another.
pub const DEFAULT_MODEL: &str = "claude-sonnet-5-5";
/// Models offered in the interface: (API name, label).
pub const MODELS: [(&str, &str); 3] = [
    ("claude-sonnet-5-5", "Sonnet 5.5 (balanced)"),
    ("claude-opus-5-5", "Opus 5.5 (most capable)"),
    ("claude-haiku-5-5", "Haiku 5.5 (fastest, cheapest)"),
];
/// The most paper text sent (characters), about 150k tokens; later pages are left out and the
/// model is told so.
pub const MAX_CHARS: usize = 600_000;
const MAX_TOKENS: u32 = 4096;

const SYSTEM: &str = "You help a researcher read an academic paper. Answer from the paper (the first document) \
and, when given, the reader's own highlights and notes (the second document). Cite the passages you rely on. \
If the paper does not answer the question, say so plainly instead of guessing, and keep anything from \
general knowledge clearly separate from what the paper says. Answer in the language of the question. \
Be concise and precise; use short paragraphs or bullet points.";

/// Who said a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

/// Where a cited passage is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A page of the paper (0-based).
    Page(usize),
    /// The reader's highlights and notes.
    Notes,
}

/// A cited passage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub source: Source,
    pub quote: String,
}

/// A run of answer text and the passages it cites.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub text: String,
    pub citations: Vec<Citation>,
}

/// One turn of a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub role: Role,
    pub segments: Vec<Segment>,
}

impl Turn {
    pub fn user(text: &str) -> Self {
        Turn { role: Role::User, segments: vec![Segment { text: text.to_owned(), citations: Vec::new() }] }
    }

    /// The turn's text without citations.
    pub fn text(&self) -> String {
        self.segments.iter().map(|s| s.text.as_str()).collect()
    }

    /// The text with page references written in, for notes: "… claim (p. 3)".
    pub fn text_with_pages(&self) -> String {
        let mut out = String::new();
        for s in &self.segments {
            out.push_str(&s.text);
            let mut pages: Vec<usize> = s.citations.iter().filter_map(|c| if let Source::Page(p) = c.source { Some(p + 1) } else { None }).collect();
            pages.sort_unstable();
            pages.dedup();
            if !pages.is_empty() {
                let list = pages.iter().map(usize::to_string).collect::<Vec<_>>().join(", ");
                out.push_str(&format!(" ({} {list})", if pages.len() == 1 { "p." } else { "pp." }));
            }
        }
        out
    }
}

/// What the question is asked about.
pub struct Context<'a> {
    pub title: &'a str,
    /// Each page's text, in order.
    pub pages: &'a [String],
    /// The reader's highlights and notes as text, or empty to leave them out.
    pub reader_notes: &'a str,
}

/// The Messages API request body for `history` (earlier turns, oldest first) and a new question.
pub fn request(model: &str, ctx: &Context, history: &[Turn], question: &str) -> Value {
    let mut blocks = Vec::new();
    let mut used = 0usize;
    let mut cut = None;
    for (i, page) in ctx.pages.iter().enumerate() {
        let text = page.trim();
        if used.saturating_add(text.len()) > MAX_CHARS {
            cut = Some(i);
            break;
        }
        used += text.len();
        // An empty page still takes its block, so block index = page index.
        blocks.push(json!({"type": "text", "text": if text.is_empty() { "(no text on this page)" } else { text }}));
    }
    let mut doc_context = format!("Content block n is page n+1 of the PDF ({} pages).", ctx.pages.len());
    if let Some(i) = cut {
        doc_context.push_str(&format!(" Pages {} and later are not included (too long).", i + 1));
    }
    let mut first: Vec<Value> = vec![json!({
        "type": "document",
        "source": {"type": "content", "content": blocks},
        "title": if ctx.title.trim().is_empty() { "The paper" } else { ctx.title.trim() },
        "context": doc_context,
        "citations": {"enabled": true},
        // Follow-up questions reuse the paper from the prompt cache.
        "cache_control": {"type": "ephemeral"},
    })];
    if !ctx.reader_notes.trim().is_empty() {
        first.push(json!({
            "type": "document",
            "source": {"type": "text", "media_type": "text/plain", "data": ctx.reader_notes.trim()},
            "title": "The reader's highlights and notes",
            "citations": {"enabled": true},
        }));
    }
    let mut messages = Vec::new();
    let mut turns: Vec<(Role, String)> = history.iter().map(|t| (t.role, t.text())).collect();
    turns.push((Role::User, question.to_owned()));
    for (k, (role, text)) in turns.into_iter().enumerate() {
        if k == 0 {
            let mut content = first.clone();
            content.push(json!({"type": "text", "text": text}));
            messages.push(json!({"role": "user", "content": content}));
        } else {
            let role = if role == Role::User { "user" } else { "assistant" };
            messages.push(json!({"role": role, "content": [{"type": "text", "text": text}]}));
        }
    }
    json!({"model": model, "max_tokens": MAX_TOKENS, "system": SYSTEM, "messages": messages})
}

/// An answer from the Messages API.
pub fn parse_answer(body: &str) -> Result<Turn> {
    let root: Value = serde_json::from_str(body).map_err(|e| Error::Lookup(format!("unreadable answer from Claude ({e})")))?;
    if let Some(err) = root.get("error") {
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("unknown error");
        return Err(Error::Lookup(format!("Claude: {msg}")));
    }
    let content = root.get("content").and_then(Value::as_array).ok_or_else(|| Error::Lookup("Claude sent no answer".into()))?;
    let mut segments: Vec<Segment> = Vec::new();
    for block in content.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("text")) {
        let text = block.get("text").and_then(Value::as_str).unwrap_or("").to_owned();
        let citations = block
            .get("citations")
            .and_then(Value::as_array)
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| {
                        let quote = c.get("cited_text").and_then(Value::as_str).unwrap_or("").trim().to_owned();
                        let source = match c.get("document_index").and_then(Value::as_u64) {
                            Some(0) => Source::Page(usize::try_from(c.get("start_block_index").and_then(Value::as_u64)?).ok()?),
                            Some(_) => Source::Notes,
                            None => return None,
                        };
                        Some(Citation { source, quote })
                    })
                    .collect()
            })
            .unwrap_or_default();
        segments.push(Segment { text, citations });
    }
    if segments.iter().all(|s| s.text.trim().is_empty()) {
        return Err(Error::Lookup("Claude sent an empty answer".into()));
    }
    Ok(Turn { role: Role::Assistant, segments })
}

/// A readable message for an API error status and body.
pub fn error_message(status: u16, body: &str) -> String {
    let detail =
        serde_json::from_str::<Value>(body).ok().and_then(|v| v.get("error")?.get("message")?.as_str().map(str::to_owned)).unwrap_or_default();
    let hint = match status {
        401 => "the API key was refused — check it in the Ask window's settings",
        403 => "this API key may not use that model",
        429 => "rate limit or spending limit reached — wait a moment, or check your Anthropic console",
        529 | 503 => "Claude is busy right now — try again shortly",
        _ => "the request failed",
    };
    if detail.is_empty() { format!("{hint} (status {status})") } else { format!("{hint}: {detail}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages() -> Vec<String> {
        vec!["Page one text.".into(), "".into(), "Page three has the result.".into()]
    }

    #[test]
    fn request_has_one_block_per_page_and_history() {
        let pages = pages();
        let ctx = Context { title: "A paper", pages: &pages, reader_notes: "Yellow: key result (p. 3)" };
        let history = vec![
            Turn::user("What is it about?"),
            Turn { role: Role::Assistant, segments: vec![Segment { text: "Testing.".into(), citations: vec![] }] },
        ];
        let r = request(DEFAULT_MODEL, &ctx, &history, "And the result?");
        let msgs = r["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        let first = msgs[0]["content"].as_array().unwrap();
        assert_eq!(first[0]["source"]["content"].as_array().unwrap().len(), 3, "empty pages keep their block");
        assert_eq!(first[0]["citations"]["enabled"], true);
        assert_eq!(first[1]["title"], "The reader's highlights and notes");
        assert_eq!(first[2]["text"], "What is it about?");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[2]["content"][0]["text"], "And the result?");
        let no_notes = request(DEFAULT_MODEL, &Context { title: "", pages: &pages, reader_notes: " " }, &[], "Q");
        assert_eq!(no_notes["messages"][0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(no_notes["messages"][0]["content"][0]["title"], "The paper");
    }

    #[test]
    fn long_papers_are_cut_and_say_so() {
        let big = vec!["x".repeat(MAX_CHARS - 10), "y".repeat(100)];
        let r = request(DEFAULT_MODEL, &Context { title: "t", pages: &big, reader_notes: "" }, &[], "q");
        assert_eq!(r["messages"][0]["content"][0]["source"]["content"].as_array().unwrap().len(), 1);
        assert!(r["messages"][0]["content"][0]["context"].as_str().unwrap().contains("Pages 2 and later"));
    }

    #[test]
    fn answers_map_citations_to_pages() {
        let body = r#"{"content":[
            {"type":"text","text":"The study finds "},
            {"type":"text","text":"difficulty rises with level","citations":[
                {"type":"content_block_location","cited_text":"Page three has the result.","document_index":0,"start_block_index":2,"end_block_index":3}]},
            {"type":"text","text":", which you flagged","citations":[
                {"type":"char_location","cited_text":"key result","document_index":1,"start_char_index":8,"end_char_index":18}]},
            {"type":"text","text":"."}]}"#;
        let t = parse_answer(body).unwrap();
        assert_eq!(t.text(), "The study finds difficulty rises with level, which you flagged.");
        assert_eq!(t.segments[1].citations[0].source, Source::Page(2));
        assert_eq!(t.segments[2].citations[0].source, Source::Notes);
        assert_eq!(t.text_with_pages(), "The study finds difficulty rises with level (p. 3), which you flagged.");
        assert!(parse_answer(r#"{"type":"error","error":{"type":"x","message":"bad key"}}"#).unwrap_err().to_string().contains("bad key"));
        assert!(parse_answer(r#"{"content":[]}"#).is_err());
        assert!(parse_answer("nope").is_err());
        assert!(error_message(401, r#"{"error":{"message":"invalid x-api-key"}}"#).contains("invalid x-api-key"));
    }
}
