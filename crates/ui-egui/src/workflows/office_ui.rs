//! Create ▸ PDF from an Office file: pick a Word, Excel, PowerPoint, OpenDocument or RTF file
//! and convert it with an installed LibreOffice into a new, unsaved document. The conversion
//! takes seconds, so it runs on a worker thread; [`PdfCraftApp::poll_office`] opens the result.

use std::sync::{Arc, mpsc};

use crate::PdfCraftApp;

/// A conversion on its way: the new document's name and where the result comes back.
pub(crate) struct OfficeJob {
    pub name: String,
    pub receiver: mpsc::Receiver<Result<Arc<Vec<u8>>, String>>,
}

impl PdfCraftApp {
    /// Create ▸ PDF from Office file…
    pub(crate) fn create_from_office_dialog(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dialog = rfd::AsyncFileDialog::new()
                .add_filter(tl!("Office files"), pdfcraft_engine::office::OFFICE_EXTENSIONS)
                .set_title(tl!("Choose an Office file"));
            self.ask(crate::pickers::Ask::File(dialog), None, |app, files| {
                if let Some(path) = files.into_iter().next() {
                    app.convert_office_file(path);
                }
            });
        }
        #[cfg(target_arch = "wasm32")]
        self.notify_tr("Converting Office files needs the desktop app");
    }

    /// Convert the Office file at `path` into a new tab (tests call this directly).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn convert_office_file(&mut self, path: std::path::PathBuf) {
        if self.office_job.is_some() {
            self.notify_tr("An Office file is already being converted");
            return;
        }
        let stem = path.file_stem().map_or_else(|| "Document".to_string(), |s| s.to_string_lossy().into_owned());
        let name = format!("{stem}.pdf");
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        let lang = crate::i18n::current();
        let work = move || {
            crate::i18n::set_current(lang);
            // A fresh session does the work: the converted bytes are opened on the UI thread.
            let session = pdfcraft_engine::Session::new();
            let result = pdfcraft_engine::guard(|| session.create_from_office(&path))
                .map_err(|_| "the conversion stopped unexpectedly".to_string())
                .and_then(|r| r.map_err(|e| tl!(&e.to_string()).to_string()));
            let _ = tx.send(result);
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        };
        if self.run_inline {
            work();
        } else if std::thread::Builder::new().name("office to pdf".into()).spawn(work).is_err() {
            self.notify_tr("The conversion could not start");
            return;
        }
        self.notify_fmt("Converting {name}…", &[("name", &stem)]);
        self.office_job = Some(OfficeJob { name, receiver: rx });
    }

    /// Open a finished conversion.
    pub(crate) fn poll_office(&mut self) {
        let Some(job) = &self.office_job else { return };
        let result = match job.receiver.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("the conversion stopped unexpectedly".to_string()),
        };
        let Some(job) = self.office_job.take() else { return };
        if let Err(e) = self.open_created_bytes(&job.name, result) {
            self.notify_fmt("Couldn't create a PDF: {e}", &[("e", &e)]);
        }
    }
}
