//! The system's Open and Save dialogs, and the PDF export they lead to. A dialog runs its own
//! message loop, so it opens with no borrow of `App` held, and its answer joins the queue.
use crate::app::{App, Effect, Incoming, Shared, push, queue_note};
use qe_api::{EncodingChoice, TextEncoding};
use qe_docs::{
    DocId, DocumentStore, OpenTarget, SavePanelRequest, WindowId, filename_with_txt_rule,
};
use qe_pdf::{PdfError, PdfJob, PdfOptions, export_worker, header_date_now};
use qe_ui::{EncodingRow, FileDialogOptions, FileDialogSetup, PdfChecks};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

/// What a dialog is for.
#[derive(Clone, Debug)]
pub enum PanelKind {
    Open {
        window: WindowId,
        rows: Vec<Option<TextEncoding>>,
    },
    Save {
        request: SavePanelRequest,
        rows: Vec<TextEncoding>,
    },
    MoveTo {
        doc: DocId,
    },
    Export {
        doc: DocId,
    },
}
#[derive(Clone, Debug)]
pub struct PanelJob {
    pub kind: PanelKind,
    pub options: FileDialogOptions,
    pub save: bool,
    pub owner: Option<HWND>,
}
#[derive(Clone, Debug)]
pub struct PanelChoice {
    pub path: String,
    pub row: Option<usize>,
    pub add_txt: Option<bool>,
    pub pdf: Option<PdfChecks>,
    pub note: Option<String>,
}

impl PanelJob {
    pub fn move_to(store: &DocumentStore, doc: DocId) -> Self {
        Self {
            kind: PanelKind::MoveTo { doc },
            options: FileDialogOptions {
                title: Some("Move To".to_owned()),
                file_name: Some(store.display_name(doc)),
                ..Default::default()
            },
            save: true,
            owner: None,
        }
    }
    pub fn export(store: &DocumentStore, doc: DocId, print: bool, checks: PdfChecks) -> Self {
        Self {
            kind: PanelKind::Export { doc },
            options: FileDialogOptions {
                title: Some(
                    if print {
                        "Print to PDF"
                    } else {
                        "Export as PDF"
                    }
                    .to_owned(),
                ),
                file_name: Some(format!("{}.pdf", store.display_name(doc))),
                pdf: Some(checks),
                ..Default::default()
            },
            save: true,
            owner: None,
        }
    }
}

/// Shows a dialog, or leaves it for the test channel.
pub fn show(shared: &Rc<Shared>, mut job: PanelJob) {
    // The dialog belongs to its document's window, wherever that tab is now.
    let owner = shared.state.try_borrow().ok().and_then(|app| {
        let doc = match &job.kind {
            PanelKind::Open { .. } => None,
            PanelKind::Save { request, .. } => Some(request.doc),
            PanelKind::MoveTo { doc } | PanelKind::Export { doc } => Some(*doc),
        };
        let window = doc
            .and_then(|doc| app.store.window_of(doc))
            .or_else(|| app.store.last_used_window())?;
        app.windows.get(&window).map(|win| win.hwnd)
    });
    job.owner = owner;
    if shared.channel {
        // It joins the queue, so it is never lost to a busy moment.
        push(shared, Incoming::TestPanel(job));
        return;
    }
    if shared.hidden {
        queue_note(shared, "A hidden run shows no file dialog.".to_owned());
        push(shared, Incoming::Panel(job, None));
        return;
    }
    // SAFETY: this is the UI thread, which stays single-threaded for COM. A repeat call only
    // counts up, and the app keeps COM until it ends.
    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if let Err(e) = init.ok() {
        queue_note(shared, format!("The file dialog could not open: {e}"));
        push(shared, Incoming::Panel(job, None));
        return;
    }
    let setup = if job.save {
        FileDialogSetup::save(&job.options)
    } else {
        FileDialogSetup::open(&job.options)
    };
    let answer = setup.and_then(|setup| setup.show(job.owner));
    match answer {
        Ok(Some(choice)) => {
            let choice = PanelChoice {
                path: choice.path,
                row: choice.encoding,
                add_txt: choice.add_txt,
                pdf: choice.pdf,
                note: choice.encoding_note,
            };
            push(shared, Incoming::Panel(job, Some(choice)));
        }
        Ok(None) => push(shared, Incoming::Panel(job, None)),
        Err(e) => {
            queue_note(shared, format!("The file dialog could not open: {e}"));
            push(shared, Incoming::Panel(job, None));
        }
    }
}

/// One running export.
pub struct PdfRun {
    pub doc: DocId,
    pub job: Option<PdfJob>,
    pub path: PathBuf,
    /// Its document closed while the job still waited for the whole text.
    pub doc_closed: bool,
}

impl App {
    pub fn open_panel(&mut self, window: WindowId) {
        let mut rows = vec![None];
        rows.extend(self.settings.enabled_encodings().into_iter().map(Some));
        let start = match self
            .settings
            .encoding_choice(qe_api::SettingKey::OpenEncoding)
        {
            EncodingChoice::Automatic => None,
            EncodingChoice::Specific(encoding) => Some(encoding),
        };
        // A setting off the list still shows, right under Automatic.
        if let Some(encoding) = start
            && !rows.contains(&Some(encoding))
        {
            rows.insert(1, Some(encoding));
        }
        let selected = rows.iter().position(|row| *row == start).unwrap_or(0);
        let names = rows
            .iter()
            .map(|row| EncodingRow::new(row.map_or("Automatic", TextEncoding::name)))
            .collect();
        self.effects.push(Effect::Panel(PanelJob {
            kind: PanelKind::Open { window, rows },
            options: FileDialogOptions {
                title: Some("Open".to_owned()),
                encodings: names,
                selected,
                ..Default::default()
            },
            save: false,
            owner: None,
        }));
    }

    /// The store wants a Save dialog. First it says which encodings can hold the text.
    pub fn needs_save_panel(&mut self, request: SavePanelRequest) {
        if request.encoding_fixed {
            self.show_save_panel(request, &[]);
            return;
        }
        let doc = request.doc;
        self.panels_waiting.push(request);
        self.store
            .check_encodings(doc, self.settings.enabled_encodings());
    }

    pub fn encoding_fit(&mut self, doc: DocId, unfit: Vec<TextEncoding>) {
        if let Some(at) = self.panels_waiting.iter().position(|r| r.doc == doc) {
            let request = self.panels_waiting.remove(at);
            self.show_save_panel(request, &unfit);
        }
    }

    fn show_save_panel(&mut self, request: SavePanelRequest, unfit: &[TextEncoding]) {
        let mut rows = self.settings.enabled_encodings();
        if !rows.contains(&request.initial_encoding) {
            rows.insert(0, request.initial_encoding);
        }
        let selected = rows
            .iter()
            .position(|row| *row == request.initial_encoding)
            .unwrap_or(0);
        let names = if request.encoding_fixed {
            Vec::new()
        } else {
            rows.iter()
                .map(|row| EncodingRow {
                    name: row.name().to_owned(),
                    enabled: !unfit.contains(row),
                })
                .collect()
        };
        let options = FileDialogOptions {
            title: Some("Save As".to_owned()),
            file_name: Some(request.suggested_name.clone()),
            encodings: names,
            selected,
            add_txt: Some(request.add_txt),
            pdf: None,
        };
        self.effects.push(Effect::Panel(PanelJob {
            kind: PanelKind::Save { request, rows },
            options,
            save: true,
            owner: None,
        }));
    }

    pub fn panel_done(&mut self, job: PanelJob, choice: Option<PanelChoice>) {
        match (job.kind, choice) {
            (PanelKind::Open { window, rows }, Some(choice)) => {
                let wanted = choice.row.and_then(|row| rows.get(row).copied());
                let encoding = match wanted {
                    Some(Some(encoding)) => Some(EncodingChoice::Specific(encoding)),
                    Some(None) => Some(EncodingChoice::Automatic),
                    None => None,
                };
                let paths = [PathBuf::from(choice.path)];
                if let Err(e) = self
                    .store
                    .open_paths(&paths, OpenTarget::Window(window), encoding)
                {
                    self.error_alert("The file could not open", &e.message);
                }
            }
            (PanelKind::Open { .. }, None) => {}
            (PanelKind::Save { request, rows }, Some(choice)) => {
                if let Some(note) = choice.note {
                    // The list could not say. Saving with a guess could lose text.
                    self.note(note);
                    self.store.panel_cancelled(request.doc);
                    return;
                }
                let path = with_txt_rule(&choice.path, choice.add_txt);
                let encoding = choice.row.and_then(|row| rows.get(row).copied());
                if let Err(e) = self.store.save_as(request.doc, path, encoding) {
                    self.error_alert("The file could not be saved", &e.message);
                }
            }
            (PanelKind::Save { request, .. }, None) => self.store.panel_cancelled(request.doc),
            (PanelKind::MoveTo { doc }, Some(choice)) => {
                if let Err(e) = self.store.move_to(doc, PathBuf::from(choice.path), true) {
                    self.error_alert("The file could not move", &e.message);
                }
            }
            (PanelKind::Export { doc }, Some(choice)) => {
                self.start_export(doc, PathBuf::from(choice.path), choice.pdf);
            }
            (PanelKind::MoveTo { .. } | PanelKind::Export { .. }, None) => {}
        }
    }

    /// How the PDF dialog's boxes start: as the last dialog left them. Wrap to Page has fixed
    /// pages, so its dialog has no rewrap box, as on the Mac.
    pub fn pdf_checks(&self, doc: DocId) -> PdfChecks {
        let key = qe_api::SettingKey::NumberPagesWhenPrinting;
        let header = self.settings.get_bool(key).unwrap_or(false);
        let key = qe_api::SettingKey::WrapToFitWhenPrinting;
        let rewrap = self.settings.get_bool(key).unwrap_or(true);
        let page_wrap = self.docs.get(&doc).is_some_and(|ui| ui.frame.page_wrap());
        PdfChecks {
            header,
            rewrap: (!page_wrap).then_some(rewrap),
        }
    }

    /// The boxes' answer goes into the settings, so that the choice sticks, as on the Mac.
    fn keep_pdf_checks(&mut self, checks: PdfChecks) {
        let keys = [
            (
                qe_api::SettingKey::NumberPagesWhenPrinting,
                Some(checks.header),
            ),
            (qe_api::SettingKey::WrapToFitWhenPrinting, checks.rewrap),
        ];
        for (key, value) in keys {
            if let Some(value) = value
                && let Err(e) = self.settings.set_bool(key, value)
            {
                self.note(format!("The PDF choice could not be kept: {e}"));
            }
        }
    }

    fn start_export(&mut self, doc: DocId, path: PathBuf, checks: Option<PdfChecks>) {
        if let Some(checks) = checks {
            self.keep_pdf_checks(checks);
        }
        let key = qe_api::SettingKey::NumberPagesWhenPrinting;
        let numbers = self.settings.get_bool(key).unwrap_or(false);
        let key = qe_api::SettingKey::WrapToFitWhenPrinting;
        let rewrap = self.settings.get_bool(key).unwrap_or(true);
        let key = qe_api::SettingKey::TabWidth;
        let tab_width = self.settings.get_integer(key).unwrap_or(8).max(1) as usize;
        let font = self.settings.font();
        let title = self.store.display_name(doc);
        let (Some(ui), Some(view)) = (self.docs.get(&doc), self.store.view(doc)) else {
            return;
        };
        let paper = ui.frame.paper();
        let options = PdfOptions {
            paper_width: paper.width,
            paper_height: paper.height,
            font_family: font.family,
            font_points: font.points,
            tab_width,
            header_and_footer: numbers,
            rewrap: ui.frame.page_wrap() || rewrap,
            window_width_dips: ui.frame.window_width_at_100_percent(view),
            title,
            date: header_date_now(),
            ..PdfOptions::default()
        };
        let snapshot = view.worker_snapshot();
        match export_worker(snapshot, options, path.clone(), Some(self.wake.clone())) {
            Ok(job) => self.pdf_runs.push(PdfRun {
                doc,
                job: Some(job),
                path,
                doc_closed: false,
            }),
            Err(e) => self.error_alert("The PDF could not be made", &e.to_string()),
        }
    }

    /// On each wake: a job that waits for a mapped file gets a newer snapshot, and a finished
    /// job reports.
    pub fn pdf_poll(&mut self) {
        let mut finished = Vec::new();
        for (at, run) in self.pdf_runs.iter_mut().enumerate() {
            let Some(job) = &run.job else {
                continue;
            };
            if job.wants_snapshot() {
                match self.store.view(run.doc) {
                    Some(view) => job.offer_snapshot(view.worker_snapshot()),
                    // No more of the text can ever come, so this job ends now.
                    None => {
                        run.doc_closed = true;
                        job.cancel();
                    }
                }
            }
            if job.is_finished() {
                finished.push(at);
            }
        }
        for at in finished.into_iter().rev() {
            let mut run = self.pdf_runs.remove(at);
            let Some(job) = run.job.take() else {
                continue;
            };
            match job.join() {
                Ok(summary) => self.notes.push(format!(
                    "PDF written: {} pages at {}",
                    summary.pages,
                    run.path.display()
                )),
                Err(PdfError::Cancelled) if run.doc_closed => self.note(format!(
                    "The PDF {} was not finished, because its document closed.",
                    run.path.display()
                )),
                Err(PdfError::Cancelled) => {}
                Err(e) => self.error_alert("The PDF could not be made", &e.to_string()),
            }
        }
    }
}

/// Runs the typed name through the `.txt` rule.
fn with_txt_rule(path: &str, add_txt: Option<bool>) -> PathBuf {
    let path = Path::new(path);
    let Some(add_txt) = add_txt else {
        return path.to_path_buf();
    };
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return path.to_path_buf();
    };
    path.with_file_name(filename_with_txt_rule(name, add_txt))
}
