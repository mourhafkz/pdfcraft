//! Study: language learning from the PDF you are reading.
//!
//! Select a paragraph and click **Study selection**: Claude returns a literal translation sentence
//! by sentence, the grammar structures worth learning (with explanations and new examples) and
//! the vocabulary worth keeping. Words go into a vocabulary deck with one click, and the deck is
//! reviewed as flashcards with spaced repetition, or exported to Anki.

use chrono::NaiveDate;
use egui::{Align, Color32, Layout, RichText, Sense, vec2};
use pdfcraft_library::ask;
use pdfcraft_library::deck::{self, Deck, Grade};
use pdfcraft_library::study::{self, Sheet, Word};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Study,
    Vocabulary,
}

/// A studied passage and what Claude made of it.
#[derive(Clone)]
pub struct Studied {
    pub passage: String,
    pub source: String,
    pub page: Option<usize>,
    pub cut: bool,
    pub sheet: Sheet,
}

/// The finished work of one Study request.
pub struct StudyResult {
    pub passage: String,
    pub source: String,
    pub page: Option<usize>,
    pub cut: bool,
    pub result: Result<Sheet, String>,
}

/// A review session: the cards still to show, and whether the answer side is up.
struct Review {
    queue: Vec<u64>,
    shown: bool,
    done: usize,
}

/// The Study window's state.
#[derive(Default)]
pub struct StudyUi {
    pub open: bool,
    tab: Tab,
    pending: bool,
    error: Option<String>,
    /// This session's sheets, newest first.
    sheets: Vec<Studied>,
    showing: usize,
    deck: Option<Deck>,
    review: Option<Review>,
    filter: String,
}

fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

impl PdfCraftApp {
    fn study_deck(&mut self) -> Option<&mut Deck> {
        if self.library.study.deck.is_none() {
            self.library.ensure_loaded();
            let dir = self.library.lib.as_ref()?.dir().to_path_buf();
            self.library.study.deck = Some(Deck::open(&dir));
        }
        self.library.study.deck.as_mut()
    }

    fn study_save_deck(&mut self) {
        if let Some(Err(e)) = self.library.study.deck.as_ref().map(Deck::save) {
            self.library.study.error = Some(format!("Couldn't save the vocabulary deck: {e}"));
        }
    }

    /// The selected text of the open document, with its title and page.
    fn study_selection(&self) -> Option<(String, String, Option<usize>)> {
        let view = self.active.and_then(|i| self.views.get(i))?;
        let text = view.selected_text()?.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.chars().count() < 2 {
            return None;
        }
        let doc = self.session.get(view.id)?;
        let name = doc
            .path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "the document".into());
        Some((text, name, Some(view.current)))
    }

    fn study_send(&mut self, passage: String, source: String, page: Option<usize>) {
        if !self.library.ask.key_loaded {
            self.library.ask.key_loaded = true;
            self.library.ask.key = crate::ask_ui::load_key();
        }
        let Some(key) = self.library.ask.key.clone() else { return };
        self.library.ensure_loaded();
        let settings = self.library.lib.as_ref().map(|l| l.settings.clone()).unwrap_or_default();
        let model = if settings.ask_model.is_empty() { ask::DEFAULT_MODEL.to_owned() } else { settings.ask_model.clone() };
        let (_, cut) = study::clip(&passage);
        let label = match page {
            Some(p) => format!("{source}, p. {}", p + 1),
            None => source.clone(),
        };
        let request = study::request(&model, &passage, &settings.study_level, &settings.study_language, &label);
        self.library.study.pending = true;
        self.library.study.error = None;
        let tx = self.library.tx.clone();
        let ctx = self.ctx.clone();
        crate::library_ui::spawn_worker(move || {
            let result = run_study(&key, &request);
            let _ = tx.send(crate::library_ui::Job::Studied(StudyResult { passage, source, page, cut, result }));
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    /// Apply a study sheet that arrived.
    pub(crate) fn study_done(&mut self, r: StudyResult) {
        self.library.study.pending = false;
        match r.result {
            Ok(sheet) => {
                self.library.study.sheets.insert(0, Studied { passage: r.passage, source: r.source, page: r.page, cut: r.cut, sheet });
                self.library.study.sheets.truncate(30);
                self.library.study.showing = 0;
                self.library.study.tab = Tab::Study;
            }
            Err(e) => self.library.study.error = Some(e),
        }
    }

    /// Show a study sheet read from a JSON file (screenshots and tests: the `study_sheet` option).
    pub fn study_show_file(&mut self, path: &str) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let sheet: Sheet = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        self.library.study.open = true;
        self.study_done(StudyResult { passage: "Beispieltext".into(), source: "sample".into(), page: Some(0), cut: false, result: Ok(sheet) });
        Ok(())
    }

    /// Show the Vocabulary tab, or start a review (`study_tab` option: `vocabulary`, `review`).
    pub fn study_show_tab(&mut self, tab: &str) {
        self.library.study.open = true;
        self.library.study.tab = Tab::Vocabulary;
        if tab == "review" {
            let due = self.study_deck().map(|d| d.due(today())).unwrap_or_default();
            self.library.study.review = Some(Review { queue: due, shown: false, done: 0 });
        }
    }

    fn study_add_word(&mut self, word: Word, source: &str, page: Option<usize>) -> bool {
        let added = self.study_deck().is_some_and(|d| d.add(word, source, page, today()));
        if added {
            self.study_save_deck();
        }
        added
    }
}

#[cfg(feature = "library-fetch")]
fn run_study(key: &str, request: &serde_json::Value) -> Result<Sheet, String> {
    pdfcraft_library::fetch::study(key, request).map_err(|e| e.to_string())
}

#[cfg(not(feature = "library-fetch"))]
fn run_study(_key: &str, _request: &serde_json::Value) -> Result<Sheet, String> {
    Err("this build can't reach Claude".into())
}

/// The Study window.
pub fn window(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if !app.library.study.open {
        return;
    }
    if !app.library.ask.key_loaded {
        app.library.ask.key_loaded = true;
        app.library.ask.key = crate::ask_ui::load_key();
    }
    let t = Tokens::get(ctx);
    let mut open = true;
    egui::Window::new("Study")
        .id(egui::Id::new("study_window"))
        .open(&mut open)
        .default_size(vec2(480.0, 680.0))
        .default_pos(ctx.content_rect().right_top() + vec2(-510.0, 100.0))
        .resizable(true)
        .show(ctx, |ui| body(app, ui, &t));
    if !open {
        app.library.study.open = false;
    }
}

fn body(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let due = app.study_deck().map(|d| d.due(today()).len()).unwrap_or(0);
    let total = app.library.study.deck.as_ref().map(|d| d.cards.len()).unwrap_or(0);
    ui.horizontal(|ui| {
        for (tab, label) in [
            (Tab::Study, "Study".to_owned()),
            (Tab::Vocabulary, if due > 0 { format!("Vocabulary ({due} due)") } else { format!("Vocabulary ({total})") }),
        ] {
            let on = app.library.study.tab == tab;
            let text = RichText::new(label).font(if on { theme::semibold(13.5) } else { theme::regular(13.5) }).color(if on {
                t.text
            } else {
                t.text_muted
            });
            let resp = ui.add(egui::Button::new(text).frame(false));
            if on {
                ui.painter().hline(resp.rect.x_range(), resp.rect.bottom() + 3.0, egui::Stroke::new(2.0, t.accent));
            }
            if resp.clicked() {
                app.library.study.tab = tab;
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(lib) = app.library.lib.as_mut() {
                let mut level = lib.settings.study_level.clone();
                egui::ComboBox::from_id_salt("study_level").selected_text(format!("Level {level}")).width(110.0).show_ui(ui, |ui| {
                    for l in ["A2–B1", "B1–B2", "B2–C1", "C1+"] {
                        ui.selectable_value(&mut level, l.to_owned(), l);
                    }
                });
                if level != lib.settings.study_level {
                    lib.settings.study_level = level;
                    lib.mark_changed();
                    let _ = lib.save();
                }
            }
        });
    });
    ui.separator();
    match app.library.study.tab {
        Tab::Study => study_tab(app, ui, t),
        Tab::Vocabulary => vocabulary_tab(app, ui, t),
    }
}

fn study_tab(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if app.library.ask.key.is_none() {
        crate::ask_ui::key_panel(app, ui, t);
        return;
    }
    let selection = app.study_selection();
    ui.horizontal(|ui| {
        let can = selection.is_some() && !app.library.study.pending;
        let resp = ui
            .add_enabled(
                can,
                egui::Button::new(RichText::new("Study selection").font(theme::medium(13.0)).color(Color32::WHITE)).fill(t.accent).corner_radius(6),
            )
            .on_disabled_hover_text("Select a paragraph in the PDF first (the text-select tool, or drag over the text)");
        if resp.clicked()
            && let Some((text, source, page)) = selection.clone()
        {
            app.study_send(text, source, page);
        }
        if app.library.study.pending {
            ui.spinner();
            ui.label(RichText::new("Studying the passage…").color(t.text_muted));
        }
    });
    match &selection {
        Some((text, _, _)) => {
            let preview: String = text.chars().take(160).collect();
            let more = if text.chars().count() > 160 { "…" } else { "" };
            ui.label(RichText::new(format!("Selected: „{preview}{more}“")).font(theme::regular(11.5)).color(t.text_faint));
        }
        None => {
            ui.label(
                RichText::new("Select a paragraph in the PDF, then click Study selection. One paragraph at a time works best.")
                    .font(theme::regular(12.0))
                    .color(t.text_faint),
            );
        }
    }
    if let Some(e) = &app.library.study.error {
        ui.label(RichText::new(format!("⚠ {e}")).color(Color32::from_rgb(0xD9, 0x4F, 0x00)));
    }
    if app.library.study.sheets.is_empty() {
        return;
    }
    // Earlier passages of this session.
    if app.library.study.sheets.len() > 1 {
        let labels: Vec<String> = app
            .library
            .study
            .sheets
            .iter()
            .map(|s| format!("{}{}", s.passage.chars().take(48).collect::<String>(), if s.passage.chars().count() > 48 { "…" } else { "" }))
            .collect();
        let current = app.library.study.showing.min(labels.len() - 1);
        egui::ComboBox::from_id_salt("study_history")
            .selected_text(labels.get(current).cloned().unwrap_or_default())
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for (i, l) in labels.iter().enumerate() {
                    ui.selectable_value(&mut app.library.study.showing, i, l);
                }
            });
    }
    ui.add_space(4.0);
    let Some(studied) = app.library.study.sheets.get(app.library.study.showing).cloned() else { return };
    let in_deck: Vec<bool> = {
        let deck = app.study_deck();
        studied.sheet.vocabulary.iter().map(|w| deck.as_ref().is_some_and(|d| d.has(&w.word))).collect()
    };
    let mut add: Vec<Word> = Vec::new();
    let mut go_to = None;
    egui::ScrollArea::vertical().id_salt("study_sheet").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal(|ui| {
            let src = match studied.page {
                Some(p) => format!("{} · p. {}", studied.source, p + 1),
                None => studied.source.clone(),
            };
            if ui.link(RichText::new(src).font(theme::regular(11.5))).clicked() {
                go_to = studied.page;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Copy as Markdown").clicked() {
                    ui.ctx().copy_text(format!("> {}\n\n{}", studied.passage, studied.sheet.to_markdown()));
                }
            });
        });
        if studied.cut {
            ui.label(RichText::new("The selection was long, so only its beginning was studied.").font(theme::regular(11.5)).color(t.text_faint));
        }

        heading(ui, t, "Translation");
        for p in &studied.sheet.translation {
            ui.label(RichText::new(p.de.trim()).font(theme::regular(13.0)).color(t.text));
            ui.label(RichText::new(p.en.trim()).font(theme::regular(12.5)).color(t.text_muted).italics());
            ui.add_space(5.0);
        }

        if !studied.sheet.grammar.is_empty() {
            heading(ui, t, "Grammar");
            for g in &studied.sheet.grammar {
                egui::Frame::NONE.fill(t.panel).corner_radius(8).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(g.structure.trim()).font(theme::semibold(13.5)).color(t.text));
                    if !g.in_text.trim().is_empty() {
                        ui.label(RichText::new(format!("„{}“", g.in_text.trim())).font(theme::regular(12.5)).color(t.accent).italics());
                    }
                    ui.label(RichText::new(g.explanation.trim()).font(theme::regular(12.5)).color(t.text));
                    for e in &g.examples {
                        ui.add_space(3.0);
                        ui.label(RichText::new(format!("• {}", e.de.trim())).font(theme::regular(12.5)).color(t.text));
                        ui.label(RichText::new(format!("   {}", e.en.trim())).font(theme::regular(12.0)).color(t.text_muted).italics());
                    }
                });
                ui.add_space(6.0);
            }
        }

        if !studied.sheet.vocabulary.is_empty() {
            ui.horizontal(|ui| {
                heading(ui, t, "Vocabulary");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let missing = in_deck.iter().filter(|x| !**x).count();
                    if missing > 0 && ui.small_button(format!("Add all {missing} to deck")).clicked() {
                        for (w, has) in studied.sheet.vocabulary.iter().zip(&in_deck) {
                            if !has {
                                add.push(w.clone());
                            }
                        }
                    }
                });
            });
            for (w, has) in studied.sheet.vocabulary.iter().zip(&in_deck) {
                egui::Frame::NONE.inner_margin(egui::Margin::symmetric(2, 4)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.set_max_width(ui.available_width() - 90.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(w.word.trim()).font(theme::semibold(13.5)).color(t.text));
                                if !w.forms.trim().is_empty() {
                                    ui.label(RichText::new(w.forms.trim()).font(theme::regular(11.5)).color(t.text_faint));
                                }
                            });
                            ui.label(RichText::new(w.meaning.trim()).font(theme::regular(12.5)).color(t.text_muted));
                            if !w.from_text.trim().is_empty() {
                                ui.label(RichText::new(format!("„{}“", w.from_text.trim())).font(theme::regular(11.5)).color(t.text_faint).italics());
                            }
                        });
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            if *has {
                                ui.label(RichText::new("✓ in deck").font(theme::regular(11.5)).color(t.text_faint));
                            } else if ui.small_button("+ Deck").clicked() {
                                add.push(w.clone());
                            }
                        });
                    });
                });
                ui.separator();
            }
        }
    });
    if !add.is_empty() {
        let n = add.len();
        let mut added = 0;
        for w in add {
            added += usize::from(app.study_add_word(w, &studied.source, studied.page));
        }
        if n > 1 {
            app.notify(format!("Added {added} words to the deck"));
        }
    }
    if let Some(p) = go_to
        && let Some(view) = app.active.and_then(|i| app.views.get_mut(i))
    {
        view.go_to_page(p);
    }
}

fn heading(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(text.to_uppercase()).font(theme::semibold(11.0)).color(t.text_faint));
    ui.add_space(3.0);
}

fn vocabulary_tab(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if app.library.study.review.is_some() {
        review(app, ui, t);
        return;
    }
    let today = today();
    let (due, total) = app.study_deck().map(|d| (d.due(today), d.cards.len())).unwrap_or_default();
    ui.horizontal(|ui| {
        let label = if due.is_empty() {
            "Nothing due today".to_owned()
        } else {
            format!("Review {} card{}", due.len(), if due.len() == 1 { "" } else { "s" })
        };
        if ui
            .add_enabled(
                !due.is_empty(),
                egui::Button::new(RichText::new(label).font(theme::medium(13.0)).color(Color32::WHITE)).fill(t.accent).corner_radius(6),
            )
            .clicked()
        {
            app.library.study.review = Some(Review { queue: due.clone(), shown: false, done: 0 });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add_enabled(total > 0, egui::Button::new("Export for Anki…"))
                .on_hover_text("A tab-separated file: in Anki, File ▸ Import")
                .clicked()
            {
                let tsv = app.library.study.deck.as_ref().map(Deck::anki_tsv).unwrap_or_default();
                let dialog = rfd::AsyncFileDialog::new().set_file_name("pdfcraft-vocabulary.txt").add_filter("Text", &["txt"]);
                app.ask_one(crate::pickers::Ask::Save(dialog), None, move |app, path| match std::fs::write(&path, tsv) {
                    Ok(()) => app.notify(format!("Saved {} — import it in Anki with File ▸ Import", path.display())),
                    Err(e) => app.notify(format!("Couldn't save: {e}")),
                });
            }
        });
    });
    ui.label(RichText::new(format!("{total} word{} in the deck", if total == 1 { "" } else { "s" })).font(theme::regular(11.5)).color(t.text_faint));
    if total == 0 {
        ui.add_space(12.0);
        ui.label(RichText::new("Words you add from a study sheet (+ Deck) collect here for review.").color(t.text_faint));
        return;
    }
    ui.add(egui::TextEdit::singleline(&mut app.library.study.filter).hint_text("Search the deck").desired_width(f32::INFINITY));
    let q = pdfcraft_library::search::fold(&app.library.study.filter);
    let mut remove = None;
    let cards = app.library.study.deck.as_ref().map(|d| d.cards.clone()).unwrap_or_default();
    egui::ScrollArea::vertical().id_salt("study_deck").auto_shrink([false, false]).show(ui, |ui| {
        for c in cards.iter().rev() {
            if !q.is_empty() && !pdfcraft_library::search::fold(&format!("{} {}", c.word.word, c.word.meaning)).contains(&q) {
                continue;
            }
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width() - 110.0);
                    ui.label(RichText::new(c.word.word.trim()).font(theme::semibold(13.0)));
                    ui.label(RichText::new(c.word.meaning.trim()).font(theme::regular(12.0)).color(t.text_muted));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icons::button(ui, "trash-2", 20.0, false, "Remove from the deck").clicked() {
                        remove = Some(c.id);
                    }
                    let next = if c.is_due(today) { "due".to_owned() } else { c.due.clone() };
                    ui.label(RichText::new(next).font(theme::regular(11.0)).color(t.text_faint));
                });
            });
            ui.separator();
        }
    });
    if let Some(id) = remove {
        if let Some(d) = app.study_deck() {
            d.remove(id);
        }
        app.study_save_deck();
    }
}

fn review(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let today = today();
    let Some(id) = app.library.study.review.as_ref().and_then(|r| r.queue.first().copied()) else {
        let done = app.library.study.review.as_ref().map(|r| r.done).unwrap_or(0);
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Done for today").font(theme::semibold(18.0)));
            ui.label(RichText::new(format!("{done} card{} reviewed", if done == 1 { "" } else { "s" })).color(t.text_muted));
            ui.add_space(10.0);
            if ui.button("Back to the deck").clicked() {
                app.library.study.review = None;
            }
        });
        return;
    };
    let Some(card) = app.study_deck().and_then(|d| d.cards.iter().find(|c| c.id == id).cloned()) else {
        if let Some(r) = app.library.study.review.as_mut() {
            r.queue.retain(|q| *q != id);
        }
        return;
    };
    let left = app.library.study.review.as_ref().map(|r| r.queue.len()).unwrap_or(0);
    let shown = app.library.study.review.as_ref().is_some_and(|r| r.shown);
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{left} left")).font(theme::regular(11.5)).color(t.text_faint));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("Stop").clicked() {
                app.library.study.review = None;
            }
        });
    });
    let mut grade = None;
    let mut reveal = false;
    egui::Frame::NONE.fill(t.panel).corner_radius(12).inner_margin(20).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(220.0);
        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.label(RichText::new(card.word.word.trim()).font(theme::semibold(24.0)).color(t.text));
            if shown {
                ui.add_space(12.0);
                ui.label(RichText::new(card.word.meaning.trim()).font(theme::regular(16.0)).color(t.text));
                if !card.word.forms.trim().is_empty() {
                    ui.label(RichText::new(card.word.forms.trim()).font(theme::regular(13.0)).color(t.text_muted));
                }
                if !card.word.from_text.trim().is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("„{}“", card.word.from_text.trim())).font(theme::regular(12.5)).color(t.text_muted).italics());
                }
                if !card.source.is_empty() {
                    let src = match card.page {
                        Some(p) => format!("{}, p. {}", card.source, p + 1),
                        None => card.source.clone(),
                    };
                    ui.label(RichText::new(src).font(theme::regular(11.0)).color(t.text_faint));
                }
            }
        });
    });
    ui.add_space(10.0);
    let keys = ui.input(|i| {
        let pressed = |k| i.key_pressed(k);
        (pressed(egui::Key::Enter), [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4].map(pressed))
    });
    if !shown {
        ui.vertical_centered(|ui| {
            if ui.add(egui::Button::new(RichText::new("Show answer  (Enter)").font(theme::medium(13.0))).min_size(vec2(200.0, 32.0))).clicked()
                || keys.0
            {
                reveal = true;
            }
        });
    } else {
        ui.columns(4, |cols| {
            for (k, g) in Grade::ALL.into_iter().enumerate() {
                let (interval, _) = card.preview(g);
                let text = format!("{}\n{}  ({})", g.label(), deck::when(interval), k + 1);
                let color = match g {
                    Grade::Again => Color32::from_rgb(0xC6, 0x3A, 0x2E),
                    Grade::Hard => Color32::from_rgb(0xC2, 0x7C, 0x0E),
                    Grade::Good => Color32::from_rgb(0x1E, 0x8E, 0x3E),
                    Grade::Easy => Color32::from_rgb(0x1A, 0x73, 0xE8),
                };
                if let Some(col) = cols.get_mut(k) {
                    let b = egui::Button::new(RichText::new(text).font(theme::medium(12.5)).color(color))
                        .min_size(vec2(col.available_width(), 44.0))
                        .sense(Sense::click());
                    if col.add(b).clicked() || keys.1.get(k).copied().unwrap_or(false) {
                        grade = Some(g);
                    }
                }
            }
        });
    }
    if reveal && let Some(r) = app.library.study.review.as_mut() {
        r.shown = true;
    }
    if let Some(g) = grade {
        if let Some(c) = app.study_deck().and_then(|d| d.card_mut(id)) {
            c.grade(g, today);
        }
        app.study_save_deck();
        if let Some(r) = app.library.study.review.as_mut() {
            r.queue.remove(0);
            r.shown = false;
            r.done += 1;
            // Forgotten cards come back at the end of today's session.
            if g == Grade::Again {
                r.queue.push(id);
            }
        }
    }
}
