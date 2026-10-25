//! The vocabulary deck: words saved while studying, reviewed as flashcards with spaced
//! repetition (a simplified SM-2), and exportable to Anki. Kept as `deck.json` in the library.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::study::Word;
use crate::{Error, Result};

const MAX_FILE: u64 = 64 << 20;

/// One flashcard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub id: u64,
    pub word: Word,
    /// Where it was found: the document's title or file name, and the page (0-based).
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub page: Option<usize>,
    #[serde(default)]
    pub added: String,
    /// The day it is next due (YYYY-MM-DD).
    pub due: String,
    /// Days until the next review after a correct answer.
    #[serde(default)]
    pub interval: f64,
    #[serde(default = "default_ease")]
    pub ease: f64,
    /// Correct answers in a row.
    #[serde(default)]
    pub reps: u32,
    #[serde(default)]
    pub lapses: u32,
}

fn default_ease() -> f64 {
    2.5
}

/// How well a card was remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    Again,
    Hard,
    Good,
    Easy,
}

impl Grade {
    pub const ALL: [Grade; 4] = [Grade::Again, Grade::Hard, Grade::Good, Grade::Easy];

    pub fn label(self) -> &'static str {
        match self {
            Grade::Again => "Again",
            Grade::Hard => "Hard",
            Grade::Good => "Good",
            Grade::Easy => "Easy",
        }
    }
}

impl Card {
    /// The interval and ease a grade would give, without changing the card (for button labels).
    pub fn preview(&self, grade: Grade) -> (f64, f64) {
        let ease = self.ease.clamp(1.3, 3.5);
        match grade {
            Grade::Again => (0.0, (ease - 0.2).max(1.3)),
            Grade::Hard => ((self.interval * 1.2).max(1.0), (ease - 0.15).max(1.3)),
            Grade::Good => (
                match self.reps {
                    0 => 1.0,
                    1 => 3.0,
                    _ => (self.interval.max(1.0) * ease).min(3650.0),
                },
                ease,
            ),
            Grade::Easy => (
                match self.reps {
                    0 => 4.0,
                    _ => (self.interval.max(1.0) * ease * 1.3).min(3650.0),
                },
                (ease + 0.15).min(3.5),
            ),
        }
    }

    /// Record an answer given on `today`.
    pub fn grade(&mut self, grade: Grade, today: NaiveDate) {
        let (interval, ease) = self.preview(grade);
        self.interval = interval;
        self.ease = ease;
        match grade {
            Grade::Again => {
                self.reps = 0;
                self.lapses = self.lapses.saturating_add(1);
            }
            _ => self.reps = self.reps.saturating_add(1),
        }
        let days = interval.round().clamp(0.0, 3650.0) as i64;
        self.due = (today + chrono::Duration::days(days)).format("%Y-%m-%d").to_string();
    }

    pub fn is_due(&self, today: NaiveDate) -> bool {
        NaiveDate::parse_from_str(&self.due, "%Y-%m-%d").map_or(true, |d| d <= today)
    }
}

/// "in 3 days", "tomorrow", "today" for an interval in days.
pub fn when(interval: f64) -> String {
    match interval.round() as i64 {
        i64::MIN..=0 => "now".into(),
        1 => "1 day".into(),
        d if d < 31 => format!("{d} days"),
        d if d < 365 => format!("{} mo", (d as f64 / 30.0).round() as i64),
        d => format!("{:.1} y", d as f64 / 365.0),
    }
}

/// The deck on disk.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Deck {
    #[serde(skip)]
    path: PathBuf,
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    pub cards: Vec<Card>,
}

impl Deck {
    /// The deck kept in the library at `dir` (empty if there is none yet or it can't be read).
    pub fn open(dir: &Path) -> Self {
        let path = dir.join("deck.json");
        let mut deck: Deck = std::fs::metadata(&path)
            .ok()
            .filter(|m| m.len() <= MAX_FILE)
            .and_then(|_| std::fs::read_to_string(&path).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        deck.next_id = deck.next_id.max(deck.cards.iter().map(|c| c.id).max().unwrap_or(0).saturating_add(1)).max(1);
        deck.path = path;
        deck
    }

    pub fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| Error::Format(e.to_string()))?;
        let tmp = self.path.with_extension(format!("json.tmp-{}", std::process::id()));
        std::fs::write(&tmp, text)?;
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(())
    }

    /// Whether the deck has this word already (case-insensitive, article included).
    pub fn has(&self, word: &str) -> bool {
        let w = word.trim();
        self.cards.iter().any(|c| c.word.word.trim().eq_ignore_ascii_case(w) || crate::search::fold(c.word.word.trim()) == crate::search::fold(w))
    }

    /// Add a word due today; returns false when it is in the deck already.
    pub fn add(&mut self, word: Word, source: &str, page: Option<usize>, today: NaiveDate) -> bool {
        if word.word.trim().is_empty() || self.has(&word.word) {
            return false;
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.cards.push(Card {
            id,
            word,
            source: source.to_owned(),
            page,
            added: crate::store::now(),
            due: today.format("%Y-%m-%d").to_string(),
            interval: 0.0,
            ease: default_ease(),
            reps: 0,
            lapses: 0,
        });
        true
    }

    pub fn remove(&mut self, id: u64) {
        self.cards.retain(|c| c.id != id);
    }

    /// Cards due on `today`, most overdue first.
    pub fn due(&self, today: NaiveDate) -> Vec<u64> {
        let mut due: Vec<&Card> = self.cards.iter().filter(|c| c.is_due(today)).collect();
        due.sort_by(|a, b| a.due.cmp(&b.due).then(a.id.cmp(&b.id)));
        due.iter().map(|c| c.id).collect()
    }

    pub fn card_mut(&mut self, id: u64) -> Option<&mut Card> {
        self.cards.iter_mut().find(|c| c.id == id)
    }

    /// Tab-separated text Anki imports (File ▸ Import): front, back, tags. HTML line breaks
    /// inside a field; tabs and newlines in the text are replaced.
    pub fn anki_tsv(&self) -> String {
        let clean = |s: &str| s.replace(['\t', '\r'], " ").replace('\n', "<br>");
        let mut out = String::from("#separator:tab\n#html:true\n#columns:Front\tBack\tTags\n");
        for c in &self.cards {
            let w = &c.word;
            let mut back = format!("<b>{}</b>", clean(&w.meanings()));
            if !w.forms.trim().is_empty() {
                back.push_str(&format!("<br>{}", clean(&w.forms)));
            }
            if !w.from_text.trim().is_empty() {
                back.push_str(&format!("<br><i>{}</i>", clean(&w.from_text)));
            }
            let tag = if w.kind.is_empty() { "pdfcraft".to_owned() } else { format!("pdfcraft {}", w.kind.replace(' ', "_")) };
            out.push_str(&format!("{}\t{}\t{}\n", clean(&w.word), back, tag));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn word(w: &str) -> Word {
        Word {
            word: w.into(),
            kind: "noun".into(),
            forms: "pl. -n".into(),
            meaning: "meaning\nline2".into(),
            meaning_ar: String::new(),
            from_text: "a\tb".into(),
        }
    }

    #[test]
    fn scheduling() {
        let mut d = Deck::default();
        assert!(d.add(word("die Aussage"), "src", Some(3), day("2026-10-25")));
        assert!(!d.add(word("Die Aussage "), "src", None, day("2026-10-25")), "no duplicates");
        let id = d.due(day("2026-10-25"))[0];
        let c = d.card_mut(id).unwrap();
        c.grade(Grade::Good, day("2026-10-25"));
        assert_eq!(c.due, "2026-10-26");
        c.grade(Grade::Good, day("2026-10-26"));
        assert_eq!(c.due, "2026-10-29");
        c.grade(Grade::Good, day("2026-10-29"));
        assert_eq!(c.due, "2026-11-06", "3 days × 2.5");
        c.grade(Grade::Again, day("2026-11-06"));
        assert_eq!((c.due.as_str(), c.reps, c.lapses), ("2026-11-06", 0, 1));
        assert!((c.ease - 2.3).abs() < 1e-9);
        assert!(d.due(day("2026-11-06")).contains(&id));
        assert!(d.due(day("2026-11-05")).is_empty());
        assert_eq!(when(0.0), "now");
        assert_eq!(when(45.0), "2 mo");
    }

    #[test]
    fn anki_export_and_storage() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-deck-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut d = Deck::open(&dir);
        d.add(word("die Aussage"), "src", None, day("2026-10-25"));
        let tsv = d.anki_tsv();
        assert!(tsv.contains("die Aussage\t<b>meaning<br>line2</b><br>pl. -n<br><i>a b</i>\tpdfcraft noun\n"));
        d.save().unwrap();
        let again = Deck::open(&dir);
        assert_eq!(again.cards.len(), 1);
        let mut again = again;
        assert!(again.add(word("das Gericht"), "", None, day("2026-10-25")));
        assert_ne!(again.cards[0].id, again.cards[1].id);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
