//! AI Assistant tools: `ai_status`, `ai_summarize`, `ai_ask` (crate `pdfcraft-ai`).
//!
//! Opt-in: without settings (an API key) every call fails before anything is sent.

use super::json::Args;
use super::{Automation, Result, ToolError, Value, failed, json};

impl Automation {
    fn ai(&self) -> Option<pdfcraft_ai::Settings> {
        self.ai_settings.clone().or_else(pdfcraft_ai::Settings::from_env)
    }

    /// Whether the assistant is set up, and with which model and endpoint (never the key).
    pub(crate) fn ai_status(&self) -> Value {
        match self.ai() {
            Some(s) => json!({ "configured": true, "model": s.model, "endpoint": s.endpoint }),
            None => json!({ "configured": false, "model": pdfcraft_ai::DEFAULT_MODEL, "endpoint": pdfcraft_ai::DEFAULT_ENDPOINT,
                "problem": pdfcraft_ai::AiError::NotConfigured.to_string() }),
        }
    }

    pub(crate) fn ai_summarize(&mut self, a: &Args) -> Result<Value> {
        let language = a.opt_str("language")?.map(str::to_string);
        self.ai_run(a, pdfcraft_ai::Task::Summarize { language })
    }

    pub(crate) fn ai_ask(&mut self, a: &Args) -> Result<Value> {
        let question = a.str("question")?.to_string();
        self.ai_run(a, pdfcraft_ai::Task::Ask { question })
    }

    fn ai_run(&mut self, a: &Args, task: pdfcraft_ai::Task) -> Result<Value> {
        let settings = self.ai().ok_or_else(|| failed(pdfcraft_ai::AiError::NotConfigured))?;
        let doc = self.doc(a)?;
        if !doc.allows_copying() {
            return Err(failed("the document's security settings don't allow copying its text, so it is not sent to the AI Assistant"));
        }
        let (id, n) = (doc.id, doc.info.pages.len());
        let pages: Vec<usize> = if a.opt_ints("pages")?.is_some() { self.pages(a, "pages")? } else { (0..n).collect() };
        let texts = self.page_texts(id, &pages)?;
        let pages: Vec<pdfcraft_ai::PageText> = pages
            .iter()
            .zip(texts)
            .map(|(p, t)| pdfcraft_ai::PageText { page: u32::try_from(p + 1).unwrap_or(u32::MAX), text: t.plain_text() })
            .collect();
        let transport = transport()?;
        let answer = pdfcraft_ai::run(&settings, &*transport, &pages, &task).map_err(|e| match e {
            pdfcraft_ai::AiError::EmptyQuestion | pdfcraft_ai::AiError::QuestionTooLong => ToolError::InvalidArgs(e.to_string()),
            e => failed(e),
        })?;
        Ok(json!({
            "text": answer.text,
            "cited_pages": answer.cited_pages,
            "pages_sent": answer.pages_sent,
            "truncated": answer.truncated,
            "model": answer.model,
            "input_tokens": answer.input_tokens,
            "output_tokens": answer.output_tokens,
        }))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn transport() -> Result<Box<dyn pdfcraft_ai::Transport>> {
    Ok(Box::new(pdfcraft_ai::HttpTransport::new(concat!("PdfCraft/", env!("CARGO_PKG_VERSION")))))
}

#[cfg(target_arch = "wasm32")]
fn transport() -> Result<Box<dyn pdfcraft_ai::Transport>> {
    Err(failed("the AI Assistant needs the desktop app"))
}
