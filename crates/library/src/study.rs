//! Language study: a passage of the PDF (usually a selected paragraph) turned into a study sheet
//! by Claude — a literal translation sentence by sentence, the grammar structures worth learning
//! with explanations and new example sentences, and the vocabulary worth keeping.
//!
//! The answer is forced through a tool with a JSON schema, so it always comes back as data the
//! interface can lay out (and save words from), never as free text to parse.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Error, Result};

/// The longest passage studied at once (characters); longer selections are cut, and the
/// interface says so.
pub const MAX_PASSAGE: usize = 12_000;
const MAX_TOKENS: u32 = 16_000;
const TOOL: &str = "study_sheet";

/// The language(s) translations and explanations are given in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    English,
    Arabic,
    Both,
}

impl Lang {
    pub const ALL: [Lang; 3] = [Lang::English, Lang::Arabic, Lang::Both];

    /// The setting's stored value and its label.
    pub fn name(self) -> &'static str {
        match self {
            Lang::English => "English",
            Lang::Arabic => "Arabic",
            Lang::Both => "English + Arabic",
        }
    }

    pub fn parse(s: &str) -> Self {
        Lang::ALL.into_iter().find(|l| l.name().eq_ignore_ascii_case(s.trim())).unwrap_or(Lang::English)
    }

    fn english(self) -> bool {
        self != Lang::Arabic
    }

    fn arabic(self) -> bool {
        self != Lang::English
    }
}

/// A sentence and its translation(s).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pair {
    pub de: String,
    pub en: String,
    /// Arabic, when asked for.
    pub ar: String,
}

impl Pair {
    /// The translations present, English first.
    pub fn translations(&self) -> Vec<&str> {
        [self.en.trim(), self.ar.trim()].into_iter().filter(|t| !t.is_empty()).collect()
    }
}

/// A grammar structure found in the passage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grammar {
    /// Its name ("Konjunktiv I in indirect speech").
    pub structure: String,
    /// Where it occurs in the passage (a short quote).
    pub in_text: String,
    pub explanation: String,
    /// The explanation in Arabic, when asked for.
    pub explanation_ar: String,
    /// New sentences that use it, with translations.
    pub examples: Vec<Pair>,
}

/// A word or phrase worth learning.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Word {
    /// The dictionary form, nouns with their article ("die Zeugenaussage").
    pub word: String,
    /// noun, verb, adjective, adverb, phrase, idiom, other.
    pub kind: String,
    /// Plural (nouns); Präteritum, Perfekt and the case or preposition it takes (verbs);
    /// register notes ("colloquial", "youth language").
    pub forms: String,
    pub meaning: String,
    /// The meaning in Arabic, when asked for.
    pub meaning_ar: String,
    /// The passage's sentence or phrase it appears in.
    pub from_text: String,
}

/// What Claude makes of a passage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sheet {
    pub translation: Vec<Pair>,
    pub grammar: Vec<Grammar>,
    pub vocabulary: Vec<Word>,
}

impl Word {
    /// The meanings present, English first, joined.
    pub fn meanings(&self) -> String {
        [self.meaning.trim(), self.meaning_ar.trim()].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" — ")
    }
}

impl Sheet {
    /// The sheet as Markdown (for copying into notes or another tool).
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("## Translation\n");
        for p in &self.translation {
            out.push_str(&format!("\n> {}\n\n{}\n", p.de.trim(), p.translations().join("\n\n")));
        }
        if !self.grammar.is_empty() {
            out.push_str("\n## Grammar\n");
            for g in &self.grammar {
                out.push_str(&format!("\n### {}\n\n*In the text:* „{}“\n", g.structure.trim(), g.in_text.trim()));
                for ex in [g.explanation.trim(), g.explanation_ar.trim()].into_iter().filter(|x| !x.is_empty()) {
                    out.push_str(&format!("\n{ex}\n"));
                }
                for e in &g.examples {
                    out.push_str(&format!("\n- {} — {}", e.de.trim(), e.translations().join(" — ")));
                }
                out.push('\n');
            }
        }
        if !self.vocabulary.is_empty() {
            out.push_str("\n## Vocabulary\n\n| Word | Forms | Meaning |\n|---|---|---|\n");
            for w in &self.vocabulary {
                out.push_str(&format!("| {} | {} | {} |\n", w.word.trim(), w.forms.trim().replace('|', "/"), w.meanings().replace('|', "/")));
            }
        }
        out
    }
}

fn system(level: &str, lang: Lang) -> String {
    let language = match lang {
        Lang::English => "English",
        Lang::Arabic => "Modern Standard Arabic",
        Lang::Both => "English and Modern Standard Arabic",
    };
    let fields = match lang {
        Lang::English => "Put translations in `en`, explanations in `explanation`, meanings in `meaning`.",
        Lang::Arabic => {
            "Write all translations, explanations and meanings in Modern Standard Arabic: translations in `ar`, explanations in `explanation_ar`, meanings in `meaning_ar`; leave the English fields empty. Keep German words and grammar terms in German inside the Arabic text."
        }
        Lang::Both => {
            "Give every translation, explanation and meaning twice: English in `en`, `explanation`, `meaning`, and Modern Standard Arabic in `ar`, `explanation_ar`, `meaning_ar`. Keep German words and grammar terms in German inside the Arabic text."
        }
    };
    let body = format!(
        "You are a German teacher helping a {level} learner study an authentic German text. Work only from the passage given. \
Translate into {language} sentence by sentence, as literally as natural {language} allows: keep the German structure \
visible, do not embellish or summarise. Pick the grammar structures a {level} learner gains most from in this passage \
(for example Konjunktiv I and II, passive forms and alternatives, extended participial attributes, nominalisation, \
Funktionsverbgefüge, modal particles, subordinate clause types, word formation, case government) — skip basics a \
{level} learner already knows. For each: name it, quote where it occurs, explain how it works in this sentence, and \
write two or three new example sentences in everyday contexts with translations. Choose vocabulary at {level} level \
and above, including idioms and colloquial or youth-language expressions; give nouns with article and plural, verbs \
with Präteritum, Perfekt and the case or preposition they take, and note the register when it matters. Always answer \
by calling the study_sheet tool exactly once (if you cannot call it, reply with only the same JSON object)."
    );
    format!("{body} {fields}")
}

fn schema(lang: Lang) -> Value {
    let mut pair_required = vec!["de"];
    let mut grammar_required = vec!["structure", "in_text", "examples"];
    let mut word_required = vec!["word", "kind", "from_text"];
    if lang.english() {
        pair_required.push("en");
        grammar_required.push("explanation");
        word_required.push("meaning");
    }
    if lang.arabic() {
        pair_required.push("ar");
        grammar_required.push("explanation_ar");
        word_required.push("meaning_ar");
    }
    let pair = json!({"type": "object", "properties": {"de": {"type": "string"}, "en": {"type": "string"}, "ar": {"type": "string"}}, "required": pair_required});
    json!({
        "type": "object",
        "properties": {
            "translation": {"type": "array", "description": "Every sentence of the passage, in order, with its translation.", "items": pair},
            "grammar": {"type": "array", "items": {"type": "object", "properties": {
                "structure": {"type": "string"},
                "in_text": {"type": "string", "description": "Short quote from the passage."},
                "explanation": {"type": "string"},
                "explanation_ar": {"type": "string"},
                "examples": {"type": "array", "items": pair}
            }, "required": grammar_required}},
            "vocabulary": {"type": "array", "items": {"type": "object", "properties": {
                "word": {"type": "string", "description": "Dictionary form; nouns with article."},
                "kind": {"type": "string", "enum": ["noun", "verb", "adjective", "adverb", "phrase", "idiom", "other"]},
                "forms": {"type": "string"},
                "meaning": {"type": "string"},
                "meaning_ar": {"type": "string"},
                "from_text": {"type": "string"}
            }, "required": word_required}}
        },
        "required": ["translation", "grammar", "vocabulary"]
    })
}

/// Cut `passage` to [`MAX_PASSAGE`] characters at a char boundary; returns it and whether it
/// was cut.
pub fn clip(passage: &str) -> (&str, bool) {
    match passage.char_indices().nth(MAX_PASSAGE) {
        Some((i, _)) => (passage.get(..i).unwrap_or(passage), true),
        None => (passage, false),
    }
}

/// The Messages API request for studying `passage`.
pub fn request(model: &str, passage: &str, level: &str, language: &str, source: &str) -> Value {
    let (passage, _) = clip(passage.trim());
    let lang = Lang::parse(language);
    json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": system(level, lang),
        "tools": [{"name": TOOL, "description": "Return the study sheet for the passage.", "input_schema": schema(lang)}],
        // "auto", not a forced tool: models that think before answering refuse a forced tool
        // choice. The system prompt asks for the tool; a JSON reply in text is accepted too.
        "tool_choice": {"type": "auto"},
        "messages": [{"role": "user", "content": format!("Passage (from {source}):\n\n{passage}")}]
    })
}

/// The study sheet in a Messages API answer.
pub fn parse(body: &str) -> Result<Sheet> {
    let root: Value = serde_json::from_str(body).map_err(|e| Error::Lookup(format!("unreadable answer from Claude ({e})")))?;
    if let Some(err) = root.get("error") {
        return Err(Error::Lookup(format!("Claude: {}", err.get("message").and_then(Value::as_str).unwrap_or("unknown error"))));
    }
    let blocks = root.get("content").and_then(Value::as_array).cloned().unwrap_or_default();
    let from_tool = blocks.iter().find(|b| b.get("type").and_then(Value::as_str) == Some("tool_use")).and_then(|b| b.get("input")).cloned();
    // Without the tool, the sheet may come as a JSON object in the text (possibly fenced).
    let from_text = || {
        let text: String = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        let start = text.find('{')?;
        let end = text.rfind('}')?;
        serde_json::from_str::<Value>(text.get(start..=end)?).ok()
    };
    let input = from_tool.or_else(from_text).ok_or_else(|| match root.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => Error::Lookup("the passage was too long to study at once; select a shorter part".into()),
        _ => Error::Lookup("Claude sent no study sheet".into()),
    })?;
    let sheet: Sheet = serde_json::from_value(input).map_err(|e| Error::Lookup(format!("the study sheet was malformed ({e})")))?;
    if sheet.translation.is_empty() && sheet.grammar.is_empty() && sheet.vocabulary.is_empty() {
        return Err(Error::Lookup("Claude sent an empty study sheet".into()));
    }
    Ok(sheet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_forces_the_tool() {
        let r = request("m", "  Er sagt, er habe keine Zeit.  ", "B2–C1", "English", "Deutsch perfekt, p. 12");
        assert_eq!(r["tool_choice"]["type"], "auto");
        assert_eq!(r["tools"][0]["name"], TOOL);
        assert_eq!(r["tools"][0]["input_schema"]["required"][0], "translation");
        assert!(r["system"].as_str().unwrap().contains("B2–C1"));
        assert_eq!(r["messages"][0]["content"], "Passage (from Deutsch perfekt, p. 12):\n\nEr sagt, er habe keine Zeit.");
        assert_eq!(r["tools"][0]["input_schema"]["properties"]["translation"]["items"]["required"], json!(["de", "en"]));
        let ar = request("m", "x", "B2–C1", "Arabic", "s");
        assert_eq!(ar["tools"][0]["input_schema"]["properties"]["translation"]["items"]["required"], json!(["de", "ar"]));
        assert!(ar["system"].as_str().unwrap().contains("Modern Standard Arabic"));
        let both = request("m", "x", "B2–C1", "English + Arabic", "s");
        assert_eq!(
            both["tools"][0]["input_schema"]["properties"]["vocabulary"]["items"]["required"],
            json!(["word", "kind", "from_text", "meaning", "meaning_ar"])
        );
        assert_eq!(Lang::parse("nonsense"), Lang::English);
    }

    #[test]
    fn clipping_is_char_safe() {
        let long = "ä".repeat(MAX_PASSAGE + 5);
        let (cut, was) = clip(&long);
        assert!(was);
        assert_eq!(cut.chars().count(), MAX_PASSAGE);
        assert_eq!(clip("kurz"), ("kurz", false));
    }

    #[test]
    fn parses_the_sheet() {
        let body = r#"{"content":[{"type":"tool_use","name":"study_sheet","input":{
            "translation":[{"de":"Er sagt, er habe keine Zeit.","en":"He says he has no time."}],
            "grammar":[{"structure":"Konjunktiv I","in_text":"er habe","explanation":"Reported speech.","examples":[{"de":"Sie meint, sie sei müde.","en":"She says she is tired."}]}],
            "vocabulary":[{"word":"die Zeugenaussage","kind":"noun","forms":"die Zeugenaussagen","meaning":"witness statement","from_text":"eine Zeugenaussage vor Gericht"}]}}],
            "stop_reason":"tool_use"}"#;
        let s = parse(body).unwrap();
        assert_eq!(s.vocabulary[0].word, "die Zeugenaussage");
        let md = s.to_markdown();
        let mut both = s.clone();
        both.vocabulary[0].meaning_ar = "شهادة".into();
        both.translation[0].ar = "يقول إنه ليس لديه وقت.".into();
        let md2 = both.to_markdown();
        assert!(md2.contains("| witness statement — شهادة |"));
        assert!(md2.contains("He says he has no time.\n\nيقول إنه ليس لديه وقت."));
        assert!(md.contains("### Konjunktiv I\n\n*In the text:* „er habe“"));
        assert!(md.contains("| die Zeugenaussage | die Zeugenaussagen | witness statement |"));
        let text_reply = r#"{"content":[{"type":"text","text":"```json\n{\"translation\":[{\"de\":\"Hallo\",\"en\":\"Hello\"}],\"grammar\":[],\"vocabulary\":[]}\n```"}]}"#;
        assert_eq!(parse(text_reply).unwrap().translation[0].en, "Hello");
        assert!(parse(r#"{"content":[],"stop_reason":"max_tokens"}"#).unwrap_err().to_string().contains("too long"));
        assert!(parse(r#"{"content":[{"type":"tool_use","input":{}}]}"#).is_err());
        assert!(parse(r#"{"error":{"message":"overloaded"}}"#).unwrap_err().to_string().contains("overloaded"));
    }
}
