//! Saved Ask conversations: one JSON file per conversation in the library's `chats/` folder,
//! found again by the PDF's path, listed per paper and exportable as Markdown.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ask::{Role, Turn};
use crate::{Error, Result};

/// The largest conversation file read.
const MAX_FILE: u64 = 32 << 20;
/// The most conversation files read from one library.
const MAX_FILES: usize = 10_000;

/// One conversation about one PDF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// Unique within the library (milliseconds since 1970 when it started, bumped on a clash).
    pub id: u64,
    /// The PDF it is about.
    pub path: String,
    /// The paper's title when the conversation started.
    #[serde(default)]
    pub title: String,
    /// RFC 3339 times.
    #[serde(default)]
    pub started: String,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub turns: Vec<Turn>,
}

impl Conversation {
    pub fn new(path: &str, title: &str) -> Self {
        let now = crate::store::now();
        let id = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
        Conversation { id, path: path.to_owned(), title: title.to_owned(), started: now.clone(), updated: now, turns: Vec::new() }
    }

    /// The first question, for lists.
    pub fn first_question(&self) -> String {
        self.turns.iter().find(|t| t.role == Role::User).map(Turn::text).unwrap_or_default()
    }

    /// The conversation as Markdown: title, reference, then each question and answer with page
    /// references written in.
    pub fn to_markdown(&self, reference: Option<&str>) -> String {
        let mut out = format!("# {}\n\n", if self.title.trim().is_empty() { "Conversation about a paper" } else { self.title.trim() });
        if let Some(r) = reference.filter(|r| !r.trim().is_empty()) {
            out.push_str(&format!("{}\n\n", r.trim()));
        }
        out.push_str(&format!("*Asked with Claude in PdfCraft, {}. File: {}*\n", self.started.get(..10).unwrap_or(&self.started), self.path));
        for t in &self.turns {
            match t.role {
                Role::User => out.push_str(&format!("\n## Q: {}\n", t.text().trim())),
                Role::Assistant => out.push_str(&format!("\n{}\n", t.text_with_pages().trim())),
            }
        }
        out
    }
}

fn dir_of(library: &Path) -> PathBuf {
    library.join("chats")
}

/// Every saved conversation, newest first. Damaged files are skipped.
pub fn load_all(library: &Path) -> Vec<Conversation> {
    let Ok(entries) = std::fs::read_dir(dir_of(library)) else { return Vec::new() };
    let mut out: Vec<Conversation> = entries
        .flatten()
        .take(MAX_FILES)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter(|e| e.metadata().is_ok_and(|m| m.len() <= MAX_FILE))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| serde_json::from_str::<Conversation>(&text).ok())
        .collect();
    out.sort_by(|a, b| b.updated.cmp(&a.updated).then(b.id.cmp(&a.id)));
    out
}

/// Write a conversation (its `updated` time is set now).
pub fn save(library: &Path, conv: &mut Conversation) -> Result<()> {
    conv.updated = crate::store::now();
    let dir = dir_of(library);
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(conv).map_err(|e| Error::Format(e.to_string()))?;
    let path = dir.join(format!("{}.json", conv.id));
    let tmp = dir.join(format!("{}.json.tmp-{}", conv.id, std::process::id()));
    std::fs::write(&tmp, text)?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Delete a saved conversation.
pub fn delete(library: &Path, id: u64) -> Result<()> {
    match std::fs::remove_file(dir_of(library).join(format!("{id}.json"))) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask::{Citation, Segment, Source};

    #[test]
    fn save_load_export_delete() {
        let d = std::env::temp_dir().join(format!("pdfcraft-chats-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mut c = Conversation::new("/p/a.pdf", "A Paper");
        c.turns.push(Turn::user("What is found?"));
        c.turns.push(Turn {
            role: Role::Assistant,
            segments: vec![
                Segment { text: "Difficulty rises".into(), citations: vec![Citation { source: Source::Page(10), quote: "q".into() }] },
                Segment { text: ".".into(), citations: vec![] },
            ],
        });
        save(&d, &mut c).unwrap();
        std::fs::write(d.join("chats").join("broken.json"), "{").unwrap();
        let all = load_all(&d);
        assert_eq!(all.len(), 1, "a damaged file is skipped");
        assert_eq!(all[0], c);
        assert_eq!(all[0].first_question(), "What is found?");
        let md = c.to_markdown(Some("Kaufmann, N. (2016). A Paper."));
        assert!(md.starts_with("# A Paper\n\nKaufmann, N. (2016). A Paper.\n"));
        assert!(md.contains("\n## Q: What is found?\n\nDifficulty rises (p. 11).\n"));
        delete(&d, c.id).unwrap();
        delete(&d, c.id).unwrap();
        assert!(load_all(&d).is_empty());
        assert!(load_all(Path::new("/nonexistent/x")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
