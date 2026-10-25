//! Ask this paper: a chat window beside the open PDF. Claude answers from the paper's text (and,
//! for a paper in the library, the reader's highlights and notes), and every answer cites the
//! pages it used; clicking a citation goes to that page.
//!
//! Uses the reader's own Anthropic API key, kept in the macOS Keychain (elsewhere a file only the
//! user can read) or read from `ANTHROPIC_API_KEY`. The paper's text leaves the machine only when
//! a question is sent, and only to Anthropic.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Align, Color32, Layout, RichText, Stroke, vec2};
use pdfcraft_library::ask::{self, Role, Source, Turn};

use crate::library_marks;
use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons};

/// The Ask window's state.
#[derive(Default)]
pub struct AskUi {
    pub open: bool,
    /// Conversations by file path.
    chats: HashMap<String, Vec<Turn>>,
    input: String,
    /// The file a question is being answered for.
    pending: Option<String>,
    /// Each file's page texts, once read.
    pages: HashMap<String, Arc<Vec<String>>>,
    key: Option<String>,
    key_loaded: bool,
    key_draft: String,
    show_key: bool,
    error: Option<String>,
    scroll_to_end: bool,
}

/// The finished work of one question.
pub struct Answer {
    pub path: String,
    pub pages: Option<Arc<Vec<String>>>,
    pub result: Result<Turn, String>,
}

#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "PdfCraft Claude API";

#[cfg(not(target_os = "macos"))]
fn key_file() -> Option<std::path::PathBuf> {
    crate::portable::data_dir().map(std::path::Path::to_path_buf).or_else(|| eframe::storage_dir("PdfCraft")).map(|d| d.join("anthropic-api-key"))
}

/// The saved API key: `ANTHROPIC_API_KEY`, else the Keychain (macOS) or the key file.
fn load_key() -> Option<String> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY")
        && !k.trim().is_empty()
    {
        return Some(k.trim().to_owned());
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-a", "default", "-w"])
            .output()
            .ok()?;
        let k = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if out.status.success() && !k.is_empty() {
            return Some(k);
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        let k = std::fs::read_to_string(key_file()?).ok()?;
        let k = k.trim();
        (!k.is_empty()).then(|| k.to_owned())
    }
}

/// Keep the API key: in the Keychain on macOS, else in a file only the user can read.
fn save_key(key: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("/usr/bin/security")
            .args(["add-generic-password", "-U", "-s", KEYCHAIN_SERVICE, "-a", "default", "-w", key])
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() { Ok(()) } else { Err("the Keychain refused the key".into()) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let path = key_file().ok_or("no settings folder")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, key).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

fn forget_key() {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("/usr/bin/security").args(["delete-generic-password", "-s", KEYCHAIN_SERVICE, "-a", "default"]).status();
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(p) = key_file() {
        let _ = std::fs::remove_file(p);
    }
}

/// The reader's highlights and notes on a paper, as the text sent with a question.
pub fn reader_notes(marks: &[library_marks::Mark], notes: &str) -> String {
    let mut out = String::new();
    if !marks.is_empty() {
        out.push_str("Highlights in the PDF (colour, page, highlighted text, the reader's comment):\n");
        for m in marks {
            let n = &m.note;
            out.push_str(&format!("- [{}, p. {}] ", if n.color.is_empty() { n.kind.as_str() } else { n.color.as_str() }, n.page + 1));
            if !n.text.is_empty() {
                out.push_str(&format!("\"{}\"", n.text));
            }
            if !n.note.is_empty() {
                out.push_str(&format!(" — comment: {}", n.note.replace('\n', " ")));
            }
            out.push('\n');
        }
    }
    if !notes.trim().is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("The reader's notes on this paper:\n");
        out.push_str(notes.trim());
    }
    out
}

impl PdfCraftApp {
    /// The open document's file path, if any.
    fn ask_active_path(&self) -> Option<String> {
        self.active.and_then(|i| self.views.get(i)).and_then(|v| self.session.get(v.id)).and_then(|d| d.path.clone())
    }

    /// Send `question` about the open document.
    fn ask_send(&mut self, question: String) {
        let Some(path) = self.ask_active_path() else { return };
        let Some(key) = self.library.ask.key.clone() else {
            self.library.ask.show_key = true;
            return;
        };
        let Some(bytes) =
            self.tab_showing(&path).and_then(|i| self.views.get(i)).and_then(|v| self.session.get(v.id)).map(|d| d.export_source().bytes)
        else {
            return;
        };
        let paper = self.library.paper_for(&path);
        let lib = self.library.lib.as_ref();
        let settings = lib.map(|l| l.settings.clone()).unwrap_or_default();
        let model = if settings.ask_model.is_empty() { ask::DEFAULT_MODEL.to_owned() } else { settings.ask_model.clone() };
        let title = paper
            .and_then(|id| lib?.get(id))
            .map(|p| p.display_title())
            .unwrap_or_else(|| std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
        let include = settings.ask_include_notes && paper.is_some();
        let notes = if include { paper.and_then(|id| lib?.get(id)).map(|p| p.notes.clone()).unwrap_or_default() } else { String::new() };
        let known_marks = if include { paper.and_then(|id| self.library.marks.get(&id).cloned()) } else { None };
        let history = self.library.ask.chats.get(&path).cloned().unwrap_or_default();
        let cached_pages = self.library.ask.pages.get(&path).cloned();
        self.library.ask.chats.entry(path.clone()).or_default().push(Turn::user(&question));
        self.library.ask.pending = Some(path.clone());
        self.library.ask.error = None;
        self.library.ask.scroll_to_end = true;
        let tx = self.library.tx.clone();
        let ctx = self.ctx.clone();
        crate::library_ui::spawn_worker(move || {
            let pages = match cached_pages {
                Some(p) => Ok(p),
                None => library_marks::page_texts(bytes.clone()).map(Arc::new),
            };
            let result = pages.clone().and_then(|pages| {
                let marks = match (include, known_marks) {
                    (false, _) => Vec::new(),
                    (true, Some(m)) => m,
                    (true, None) => library_marks::scan(bytes, false).map(|s| s.marks).unwrap_or_default(),
                };
                let reader = if include { reader_notes(&marks, &notes) } else { String::new() };
                let request = ask::request(&model, &ask::Context { title: &title, pages: &pages, reader_notes: &reader }, &history, &question);
                ask_claude(&key, &request)
            });
            let _ = tx.send(crate::library_ui::Job::Asked(Answer { path, pages: pages.ok(), result }));
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    /// Apply an answer that arrived.
    pub(crate) fn ask_answered(&mut self, a: Answer) {
        if let Some(p) = a.pages {
            self.library.ask.pages.insert(a.path.clone(), p);
        }
        if self.library.ask.pending.as_deref() == Some(a.path.as_str()) {
            self.library.ask.pending = None;
        }
        match a.result {
            Ok(turn) => self.library.ask.chats.entry(a.path).or_default().push(turn),
            Err(e) => {
                // Take the unanswered question back into the box so it can be sent again.
                if let Some(chat) = self.library.ask.chats.get_mut(&a.path)
                    && chat.last().is_some_and(|t| t.role == Role::User)
                    && let Some(q) = chat.pop()
                {
                    self.library.ask.input = q.text();
                }
                if e.contains("API key") {
                    self.library.ask.show_key = true;
                }
                self.library.ask.error = Some(e);
            }
        }
        self.library.ask.scroll_to_end = true;
    }

    /// Go to a cited page of the open document.
    fn ask_go_to(&mut self, path: &str, page: usize) {
        if let Some(i) = self.tab_showing(path)
            && let Some(view) = self.views.get_mut(i)
        {
            view.go_to_page(page);
        }
    }
}

#[cfg(feature = "library-fetch")]
fn ask_claude(key: &str, request: &serde_json::Value) -> Result<Turn, String> {
    pdfcraft_library::fetch::ask(key, request).map_err(|e| e.to_string())
}

#[cfg(not(feature = "library-fetch"))]
fn ask_claude(_key: &str, _request: &serde_json::Value) -> Result<Turn, String> {
    Err("this build can't reach Claude".into())
}

/// The Ask window over the open document.
pub fn window(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if !app.library.ask.open {
        return;
    }
    let Some(path) = app.ask_active_path() else { return };
    if !app.library.ask.key_loaded {
        app.library.ask.key_loaded = true;
        app.library.ask.key = load_key();
    }
    let t = Tokens::get(ctx);
    let mut open = true;
    egui::Window::new("Ask this paper")
        .id(egui::Id::new("ask_window"))
        .open(&mut open)
        .default_size(vec2(440.0, 620.0))
        .default_pos(ctx.content_rect().right_top() + vec2(-470.0, 110.0))
        .resizable(true)
        .show(ctx, |ui| body(app, ui, &t, &path));
    if !open {
        app.library.ask.open = false;
    }
}

fn body(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens, path: &str) {
    let in_library = app.library.paper_for(path);
    // Settings row.
    let mut model_changed = false;
    ui.horizontal(|ui| {
        if let Some(lib) = app.library.lib.as_mut() {
            let current = if lib.settings.ask_model.is_empty() { ask::DEFAULT_MODEL.to_owned() } else { lib.settings.ask_model.clone() };
            let label = ask::MODELS.iter().find(|m| m.0 == current).map(|m| m.1).unwrap_or(current.as_str()).to_owned();
            egui::ComboBox::from_id_salt("ask_model").selected_text(label).width(170.0).show_ui(ui, |ui| {
                for (id, label) in ask::MODELS {
                    if ui.selectable_label(current == id, label).clicked() {
                        lib.settings.ask_model = id.to_owned();
                        model_changed = true;
                    }
                }
            });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icons::button(ui, "settings", 22.0, app.library.ask.show_key, "API key").clicked() {
                app.library.ask.show_key = !app.library.ask.show_key;
            }
            if icons::button(ui, "plus", 22.0, false, "New conversation").clicked() {
                app.library.ask.chats.remove(path);
                app.library.ask.error = None;
            }
        });
    });
    if model_changed {
        app.library.mark_settings_changed();
    }
    if in_library.is_some()
        && let Some(lib) = app.library.lib.as_mut()
    {
        let mut include = lib.settings.ask_include_notes;
        if ui
            .add(crate::widgets::checkbox(&mut include, RichText::new("Include my highlights and notes").font(theme::regular(12.0))))
            .on_hover_text("Send your highlights, their comments and your notes on this paper with each question")
            .changed()
        {
            lib.settings.ask_include_notes = include;
            app.library.mark_settings_changed();
        }
    } else if in_library.is_none() {
        ui.label(RichText::new("Add this PDF to the Library to include your highlights and notes.").font(theme::regular(11.5)).color(t.text_faint));
    }

    if app.library.ask.key.is_none() || app.library.ask.show_key {
        key_panel(app, ui, t);
        if app.library.ask.key.is_none() {
            return;
        }
    }
    ui.separator();

    // Conversation.
    let turns = app.library.ask.chats.get(path).cloned().unwrap_or_default();
    let pending = app.library.ask.pending.as_deref() == Some(path);
    let mut go_to = None;
    let mut ask_now = None;
    let mut save = None;
    let input_h = 86.0;
    egui::ScrollArea::vertical()
        .id_salt(("ask_scroll", path))
        .max_height((ui.available_height() - input_h).max(80.0))
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if turns.is_empty() && !pending {
                ui.add_space(6.0);
                ui.label(RichText::new("Ask anything about this paper. Answers cite the pages they come from.").color(t.text_muted));
                ui.add_space(6.0);
                let mut suggestions = vec![
                    "Summarise the main findings in five bullet points.",
                    "What method, data and sample were used?",
                    "What are the limitations the authors acknowledge?",
                    "Which claims would I need to check before citing this paper?",
                ];
                if in_library.is_some() {
                    suggestions.push("Go through my highlights and explain why each one matters.");
                }
                for s in suggestions {
                    if ui.add(egui::Button::new(RichText::new(s).font(theme::regular(12.5))).wrap()).clicked() {
                        ask_now = Some(s.to_owned());
                    }
                }
            }
            for (k, turn) in turns.iter().enumerate() {
                ui.add_space(6.0);
                match turn.role {
                    Role::User => {
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            egui::Frame::NONE.fill(t.accent_soft).corner_radius(10).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                                ui.set_max_width(320.0);
                                ui.label(RichText::new(turn.text()).font(theme::regular(13.0)));
                            });
                        });
                    }
                    Role::Assistant => {
                        answer(ui, t, turn, &mut go_to);
                        ui.horizontal(|ui| {
                            if ui.small_button("Copy").clicked() {
                                ui.ctx().copy_text(turn.text_with_pages());
                            }
                            if in_library.is_some()
                                && ui
                                    .small_button("Save to notes")
                                    .on_hover_text("Append the question and this answer to the paper's notes")
                                    .clicked()
                            {
                                let q = turns.get(k.wrapping_sub(1)).filter(|q| q.role == Role::User).map(Turn::text).unwrap_or_default();
                                save = Some((q, turn.text_with_pages()));
                            }
                        });
                    }
                }
            }
            if pending {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Reading the paper and thinking…").color(t.text_muted));
                });
            }
            if let Some(e) = &app.library.ask.error {
                ui.add_space(6.0);
                ui.label(RichText::new(format!("⚠ {e}")).color(Color32::from_rgb(0xD9, 0x4F, 0x00)));
            }
        });

    // Input.
    ui.separator();
    let resp = ui.add(
        egui::TextEdit::multiline(&mut app.library.ask.input)
            .hint_text("Ask about this paper…  (Enter to send, Shift+Enter for a new line)")
            .desired_rows(2)
            .desired_width(f32::INFINITY),
    );
    let enter = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
    ui.horizontal(|ui| {
        let can = !pending && !app.library.ask.input.trim().is_empty();
        let send = ui.add_enabled(
            can,
            egui::Button::new(RichText::new("Ask").font(theme::medium(13.0)).color(Color32::WHITE)).fill(t.accent).corner_radius(6),
        );
        if (send.clicked() || enter) && can {
            let q = app.library.ask.input.trim().to_owned();
            app.library.ask.input.clear();
            ask_now = Some(q);
        }
        ui.label(RichText::new("Paper text is sent to Anthropic only when you ask.").font(theme::regular(11.0)).color(t.text_faint));
    });
    if enter {
        // Drop the newline the key typed.
        let trimmed = app.library.ask.input.trim_end_matches('\n').to_owned();
        app.library.ask.input = trimmed;
    }
    if let Some(q) = ask_now
        && !pending
    {
        app.ask_send(q);
    }
    if let Some(page) = go_to {
        app.ask_go_to(path, page);
    }
    if let Some((q, a)) = save
        && let Some(id) = in_library
        && let Some(p) = app.library.lib.as_mut().and_then(|l| l.get_mut(id))
    {
        if !p.notes.trim().is_empty() {
            p.notes.push_str("\n\n");
        }
        p.notes.push_str(&format!("**Q:** {q}\n\n{a}"));
        app.library.note_edited(id);
        app.notify("Saved to the paper's notes");
    }
    let _ = Stroke::NONE;
}

/// An answer with clickable page citations after the sentences that use them.
fn answer(ui: &mut egui::Ui, t: &Tokens, turn: &Turn, go_to: &mut Option<usize>) {
    egui::Frame::NONE.fill(t.panel).corner_radius(10).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for seg in &turn.segments {
                // Line breaks inside a wrapped row: break the row at each one.
                let mut parts = seg.text.split('\n').peekable();
                while let Some(part) = parts.next() {
                    if !part.is_empty() {
                        ui.label(RichText::new(part).font(theme::regular(13.0)).color(t.text));
                    }
                    if parts.peek().is_some() {
                        ui.end_row();
                    }
                }
                let mut seen: Vec<String> = Vec::new();
                for c in &seg.citations {
                    let (label, page) = match c.source {
                        Source::Page(p) => (format!("p.{}", p + 1), Some(p)),
                        Source::Notes => ("your notes".to_owned(), None),
                    };
                    if seen.contains(&label) {
                        continue;
                    }
                    seen.push(label.clone());
                    ui.add_space(3.0);
                    let chip = egui::Button::new(RichText::new(label).font(theme::medium(11.0)).color(t.accent))
                        .fill(t.accent_soft)
                        .corner_radius(8)
                        .small();
                    let quote =
                        if c.quote.chars().count() > 300 { format!("{}…", c.quote.chars().take(300).collect::<String>()) } else { c.quote.clone() };
                    let resp = ui.add(chip).on_hover_text(format!("“{quote}”"));
                    if resp.clicked()
                        && let Some(p) = page
                    {
                        *go_to = Some(p);
                    }
                }
            }
        });
    });
}

fn key_panel(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    ui.add_space(6.0);
    egui::Frame::NONE.fill(t.panel).corner_radius(8).inner_margin(10).show(ui, |ui| {
        ui.label(RichText::new("Claude API key").strong());
        ui.label(
            RichText::new(
                "Create one at console.anthropic.com → API keys (pay per use; a question about a paper usually costs a few cents). \
It is kept in your macOS Keychain.",
            )
            .font(theme::regular(12.0))
            .color(t.text_muted),
        );
        if app.library.ask.key.is_some() {
            ui.label(RichText::new("A key is saved.").font(theme::regular(12.0)).color(t.text_muted));
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.library.ask.key_draft).password(true).hint_text("sk-ant-…").desired_width(220.0));
            let ok = app.library.ask.key_draft.trim().starts_with("sk-");
            if ui.add_enabled(ok, egui::Button::new("Save")).clicked() {
                let k = app.library.ask.key_draft.trim().to_owned();
                match save_key(&k) {
                    Ok(()) => {
                        app.library.ask.key = Some(k);
                        app.library.ask.key_draft.clear();
                        app.library.ask.show_key = false;
                        app.library.ask.error = None;
                    }
                    Err(e) => app.library.ask.error = Some(format!("Couldn't save the key: {e}")),
                }
            }
            if app.library.ask.key.is_some() && ui.button("Forget").clicked() {
                forget_key();
                app.library.ask.key = None;
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfcraft_library::cite::HighlightNote;

    #[test]
    fn reader_notes_list_highlights_then_notes() {
        let marks = vec![library_marks::Mark {
            note: HighlightNote {
                page: 2,
                kind: "Highlight".into(),
                color: "Yellow".into(),
                text: "key result".into(),
                note: "for chapter 3".into(),
            },
            rgb: [1.0, 1.0, 0.0],
        }];
        let s = reader_notes(&marks, "My summary.");
        assert!(s.contains("- [Yellow, p. 3] \"key result\" — comment: for chapter 3"));
        assert!(s.ends_with("The reader's notes on this paper:\nMy summary."));
        assert_eq!(reader_notes(&[], "  "), "");
    }
}
