//! The AI Assistant panel: summarize the open document, or ask it a question, and follow the
//! answer's page citations. Opt-in: nothing is sent until the user has added an Anthropic API
//! key in Preferences ▸ AI Assistant (or set `ANTHROPIC_API_KEY`), and then only when they ask.
//!
//! The request runs on a worker thread: it reads the pages' text and calls the service, which
//! takes seconds. [`PdfCraftApp::poll_ai`] picks the answer up each frame.

use std::sync::mpsc;

use pdfcraft_engine::DocId;

use crate::theme::Tokens;
use crate::{PdfCraftApp, theme, widgets};

/// The most exchanges a document's history keeps.
const HISTORY: usize = 20;

/// One question (or summary request) and its answer.
pub(crate) struct Exchange {
    pub doc: DocId,
    /// `None` for a summary.
    pub question: Option<String>,
    pub answer: Result<pdfcraft_ai::Answer, String>,
}

/// A request on its way.
pub(crate) struct AiRun {
    pub doc: DocId,
    pub question: Option<String>,
    pub receiver: mpsc::Receiver<Result<pdfcraft_ai::Answer, String>>,
}

/// The panel's state, kept on the app.
#[derive(Default)]
pub(crate) struct AiPanel {
    pub question: String,
    pub history: Vec<Exchange>,
    pub run: Option<AiRun>,
}

/// What the panel asks the app to do.
pub(crate) enum PanelAction {
    Summarize,
    Ask(String),
    GoTo(usize),
    Preferences,
    Clear,
}

/// The settings to use: the key from Preferences, else the environment's.
pub(crate) fn settings(key: &str, model: &str, endpoint: Option<&str>) -> Option<pdfcraft_ai::Settings> {
    let mut s = pdfcraft_ai::Settings::with_key(key).or_else(pdfcraft_ai::Settings::from_env)?;
    let model = model.trim();
    if !model.is_empty() {
        s.model = model.to_string();
    }
    if let Some(e) = endpoint {
        s.endpoint = e.to_string();
    }
    Some(s)
}

/// The AI Assistant panel.
pub(crate) fn panel(ui: &mut egui::Ui, t: &Tokens, state: &mut AiPanel, id: DocId, configured: bool, copying: bool) -> Option<PanelAction> {
    let mut action = None;
    if cfg!(target_arch = "wasm32") {
        ui.label(egui::RichText::new(tl!("The AI Assistant needs the desktop app.")).color(t.text_muted));
        return None;
    }
    if !configured {
        ui.label(tl!("Summarize this document or ask it questions with Claude. Answers cite the pages they come from."));
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(tl!("Add your Anthropic API key first. The document's text is sent to Anthropic only when you ask."))
                .small()
                .color(t.text_muted),
        );
        ui.add_space(8.0);
        if ui.button(tl!("Set up the AI Assistant…")).clicked() {
            action = Some(PanelAction::Preferences);
        }
        return action;
    }
    if !copying {
        ui.label(
            egui::RichText::new(tl!("This document's security settings don't allow copying its text, so the AI Assistant can't read it."))
                .color(t.text_muted),
        );
        return None;
    }
    let busy = state.run.as_ref().is_some_and(|r| r.doc == id);
    ui.horizontal(|ui| {
        if ui.add_enabled(!busy, egui::Button::new(tl!("Summarize"))).clicked() {
            action = Some(PanelAction::Summarize);
        }
        if state.history.iter().any(|e| e.doc == id) && ui.add_enabled(!busy, egui::Button::new(tl!("Clear"))).clicked() {
            action = Some(PanelAction::Clear);
        }
    });
    ui.add_space(6.0);
    let label = ui.label(egui::RichText::new(tl!("Question")).small().color(t.text_muted));
    let edit = ui
        .add(
            egui::TextEdit::multiline(&mut state.question)
                .hint_text(tl!("Ask about this document…"))
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .char_limit(pdfcraft_ai::MAX_QUESTION_CHARS),
        )
        .labelled_by(label.id);
    // Enter asks; Shift+Enter starts a new line.
    let enter = edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
    let ready = !busy && !state.question.trim().is_empty();
    ui.horizontal(|ui| {
        if (ui.add_enabled(ready, egui::Button::new(tl!("Ask"))).clicked() || enter && ready) && action.is_none() {
            action = Some(PanelAction::Ask(state.question.trim().to_string()));
        }
        if busy {
            ui.spinner();
            ui.label(egui::RichText::new(tl!("Reading the document…")).color(t.text_muted));
        }
    });
    ui.add_space(8.0);
    // Newest first.
    for e in state.history.iter().rev().filter(|e| e.doc == id) {
        egui::Frame::new().fill(t.hover).corner_radius(egui::CornerRadius::same(6)).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let asked = e.question.clone().unwrap_or_else(|| tl!("Summary").to_string());
            ui.label(egui::RichText::new(asked).font(theme::semibold(13.0)));
            ui.add_space(4.0);
            match &e.answer {
                Ok(a) => {
                    ui.add(egui::Label::new(a.text.as_str()).selectable(true).wrap());
                    if !a.cited_pages.is_empty() {
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new(tl!("Pages:")).small().color(t.text_muted));
                            for p in &a.cited_pages {
                                if widgets::pill_button(ui, &crate::i18n::fmt(tl!("p. {n}"), &[("n", &p.to_string())]), false).clicked() {
                                    action = Some(PanelAction::GoTo((*p as usize).saturating_sub(1)));
                                }
                            }
                        });
                    }
                    if a.truncated {
                        ui.label(egui::RichText::new(tl!("The document is long: only its first pages were read.")).small().color(t.text_muted));
                    }
                }
                Err(m) => {
                    ui.label(egui::RichText::new(m.as_str()).color(ui.visuals().error_fg_color));
                }
            }
        });
        ui.add_space(6.0);
    }
    action
}

impl PdfCraftApp {
    /// Apply what the AI Assistant panel asked for.
    pub(crate) fn ai_action(&mut self, index: usize, action: PanelAction) {
        match action {
            PanelAction::GoTo(p) => {
                if let Some(v) = self.views.get_mut(index) {
                    v.go_to_page(p);
                }
            }
            PanelAction::Preferences => self.run_command("app.preferences"),
            PanelAction::Clear => {
                if let Some((_, id)) = self.active_ids() {
                    self.ai.history.retain(|e| e.doc != id);
                }
            }
            PanelAction::Summarize => self.start_ai(None),
            PanelAction::Ask(q) => {
                self.ai.question.clear();
                self.start_ai(Some(q));
            }
        }
    }

    /// Start a request for the active document on a worker thread.
    fn start_ai(&mut self, question: Option<String>) {
        let Some((_, id)) = self.active_ids() else { return };
        let Some(settings) = settings(&self.ai_key, &self.ai_model, self.ai_endpoint.as_deref()) else {
            self.run_command("app.preferences");
            return;
        };
        let Some(doc) = self.session.get(id) else { return };
        if !doc.allows_copying() {
            return;
        }
        let source = doc.export_source();
        let pages = doc.info.pages.len();
        let task = match &question {
            Some(q) => pdfcraft_ai::Task::Ask { question: q.clone() },
            None => pdfcraft_ai::Task::Summarize { language: Some(language_name(crate::i18n::current().code()).to_string()) },
        };
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let lang = crate::i18n::current();
        let work = move || {
            crate::i18n::set_current(lang);
            let result = ask(source, pages, &settings, &task);
            let _ = tx.send(result);
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.run_inline {
                work();
            } else if std::thread::Builder::new().name("ai assistant".into()).spawn(work).is_err() {
                self.notify_error(tl!("The AI Assistant could not start."));
                return;
            }
        }
        #[cfg(target_arch = "wasm32")]
        work();
        self.ai.run = Some(AiRun { doc: id, question, receiver: rx });
    }

    /// Pick up a finished answer.
    pub(crate) fn poll_ai(&mut self) {
        let Some(run) = &self.ai.run else { return };
        let answer = match run.receiver.try_recv() {
            Ok(a) => a,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(tl!("The AI Assistant stopped unexpectedly.").to_string()),
        };
        let Some(run) = self.ai.run.take() else { return };
        self.ai.history.push(Exchange { doc: run.doc, question: run.question, answer });
        let overflow = self.ai.history.iter().filter(|e| e.doc == run.doc).count().saturating_sub(HISTORY);
        if overflow > 0 {
            let mut left = overflow;
            self.ai.history.retain(|e| {
                let drop = e.doc == run.doc && left > 0;
                if drop {
                    left -= 1;
                }
                !drop
            });
        }
    }
}

/// The English name of an interface language for the summary prompt.
fn language_name(code: &str) -> &'static str {
    match code {
        "fi" => "Finnish",
        "sv" => "Swedish",
        "de" => "German",
        "fr" => "French",
        "es" => "Spanish",
        "it" => "Italian",
        "ja" => "Japanese",
        "zh-Hans" | "zh" => "Simplified Chinese",
        "ru" => "Russian",
        "pl" => "Polish",
        _ => "English",
    }
}

/// Read the pages' text and ask the model (on the worker thread).
fn ask(
    source: pdfcraft_engine::export::ExportSource,
    pages: usize,
    settings: &pdfcraft_ai::Settings,
    task: &pdfcraft_ai::Task,
) -> Result<pdfcraft_ai::Answer, String> {
    let mut exporter = pdfcraft_engine::export::Exporter::from_source(source);
    let mut texts = Vec::new();
    let mut chars = 0usize;
    for p in 0..pages {
        // Pages past the request's budget would be left out anyway: stop reading there.
        if chars > pdfcraft_ai::MAX_INPUT_CHARS {
            break;
        }
        let text = exporter.text(p).unwrap_or_default();
        chars = chars.saturating_add(text.chars().count());
        texts.push(pdfcraft_ai::PageText { page: u32::try_from(p + 1).unwrap_or(u32::MAX), text });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let transport = pdfcraft_ai::HttpTransport::new(concat!("PdfCraft/", env!("CARGO_PKG_VERSION")));
        pdfcraft_ai::run(settings, &transport, &texts, task).map_err(|e| tl!(&e.to_string()).to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (settings, task, texts);
        Err("the AI Assistant needs the desktop app".into())
    }
}
