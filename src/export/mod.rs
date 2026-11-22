pub mod docx;
pub mod html;
pub mod ir;
pub mod json;
pub mod pdf;
pub mod print;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use ropey::Rope;

use crate::document::ast::parse_to_ir;
use crate::export::print::PrintJobOptions;

/// Supported background export / print job kinds.
#[derive(Debug, Clone)]
pub enum ExportTarget {
    Pdf(PathBuf),
    PdfWithOptions {
        path: PathBuf,
        layout: pdf::PdfLayoutOptions,
    },
    Docx(PathBuf),
    Html(PathBuf),
    Json {
        path: PathBuf,
        source_path: Option<PathBuf>,
    },
    Print { spool_only: bool },
    PrintWithOptions(PrintJobOptions),
}

/// Progress/completion messages streamed back from background worker threads.
#[derive(Debug, Clone)]
pub enum ExportEvent {
    Started { label: String },
    Progress { fraction: f32, stage: String },
    Completed { message: String, output_path: Option<PathBuf> },
    Failed { error: String },
}

struct ExportRequest {
    rope: Rope,
    title: String,
    target: ExportTarget,
    base_dir: Option<PathBuf>,
}

/// Non-blocking export & print pipeline running on a dedicated `std::thread` worker.
pub struct BackgroundExporter {
    tx: Sender<ExportRequest>,
    rx: Receiver<ExportEvent>,
    pub busy: bool,
    pub progress: f32,
    pub status_text: String,
}

impl Default for BackgroundExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundExporter {
    pub fn new() -> Self {
        Self::spawn_with_ctx(None)
    }

    pub fn with_repaint_ctx(ctx: egui::Context) -> Self {
        Self::spawn_with_ctx(Some(ctx))
    }

    fn spawn_with_ctx(repaint_ctx: Option<egui::Context>) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<ExportRequest>();
        let (ev_tx, ev_rx) = mpsc::channel::<ExportEvent>();
        let notify = move || {
            if let Some(ref c) = repaint_ctx {
                c.request_repaint();
            }
        };

        thread::Builder::new()
            .name("md-exporter".to_string())
            .spawn(move || {
                while let Ok(req) = req_rx.recv() {
                    let label = match &req.target {
                        ExportTarget::Pdf(p) | ExportTarget::PdfWithOptions { path: p, .. } => {
                            format!("Exporting PDF to {}", p.display())
                        }
                        ExportTarget::Docx(p) => format!("Exporting DOCX to {}", p.display()),
                        ExportTarget::Html(p) => format!("Exporting HTML to {}", p.display()),
                        ExportTarget::Json { path, .. } => {
                            format!("Exporting JSON to {}", path.display())
                        }
                        ExportTarget::Print { .. } | ExportTarget::PrintWithOptions(_) => {
                            "Preparing print spool PDF...".to_string()
                        }
                    };
                    let _ = ev_tx.send(ExportEvent::Started { label });
                    notify();
                    let _ = ev_tx.send(ExportEvent::Progress {
                        fraction: 0.15,
                        stage: "Parsing Markdown AST...".to_string(),
                    });
                    notify();

                    let markdown = req.rope.to_string();
                    let blocks = parse_to_ir(&markdown);

                    let base_dir = req.base_dir.as_deref();
                    let result: Result<(String, Option<PathBuf>), String> = match req.target {
                        ExportTarget::Pdf(path) => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = 0i32;
                            pdf::export_pdf_to_file_with_options_and_base_dir(
                                &blocks,
                                &req.title,
                                &path,
                                pdf::PdfLayoutOptions::default(),
                                base_dir,
                                |f| {
                                    let _ = tx_prog.send(ExportEvent::Progress {
                                        fraction: 0.2 + 0.75 * f,
                                        stage: "Compiling vector PDF pages...".to_string(),
                                    });
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        notify();
                                    }
                                },
                            )
                            .map(|()| (format!("Saved PDF: {}", path.display()), Some(path)))
                        }
                        ExportTarget::PdfWithOptions { path, layout } => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = 0i32;
                            pdf::export_pdf_to_file_with_options_and_base_dir(
                                &blocks,
                                &req.title,
                                &path,
                                layout,
                                base_dir,
                                |f| {
                                    let _ = tx_prog.send(ExportEvent::Progress {
                                        fraction: 0.2 + 0.75 * f,
                                        stage: "Compiling vector PDF pages...".to_string(),
                                    });
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        notify();
                                    }
                                },
                            )
                            .map(|()| (format!("Saved PDF: {}", path.display()), Some(path)))
                        }
                        ExportTarget::Docx(path) => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = 0i32;
                            docx::export_docx_to_file_with_base_dir(
                                &blocks,
                                &path,
                                base_dir,
                                |f| {
                                    let _ = tx_prog.send(ExportEvent::Progress {
                                        fraction: 0.2 + 0.75 * f,
                                        stage: "Building OpenXML Word document...".to_string(),
                                    });
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        notify();
                                    }
                                },
                            )
                            .map(|()| (format!("Saved Word document: {}", path.display()), Some(path)))
                        }
                        ExportTarget::Html(path) => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = 0i32;
                            html::export_html_to_file_with_base_dir(
                                &blocks,
                                &req.title,
                                &path,
                                base_dir,
                                |f| {
                                    let _ = tx_prog.send(ExportEvent::Progress {
                                        fraction: 0.2 + 0.75 * f,
                                        stage: "Rendering standalone HTML...".to_string(),
                                    });
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        notify();
                                    }
                                },
                            )
                            .map(|()| (format!("Saved HTML: {}", path.display()), Some(path)))
                        }
                        ExportTarget::Json { path, source_path } => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = 0i32;
                            json::export_json_to_file(
                                &blocks,
                                &req.title,
                                source_path.as_deref(),
                                &markdown,
                                &path,
                                |f| {
                                    let _ = tx_prog.send(ExportEvent::Progress {
                                        fraction: 0.2 + 0.75 * f,
                                        stage: "Serializing structured JSON...".to_string(),
                                    });
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        notify();
                                    }
                                },
                            )
                            .map(|()| (format!("Saved JSON: {}", path.display()), Some(path)))
                        }
                        ExportTarget::Print { spool_only } => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = 0i32;
                            match print::create_print_spool_pdf_with_options_and_base_dir(
                                &blocks,
                                &req.title,
                                &PrintJobOptions::default(),
                                base_dir,
                                |f| {
                                    let _ = tx_prog.send(ExportEvent::Progress {
                                        fraction: 0.2 + 0.65 * f,
                                        stage: "Rendering print spool PDF...".to_string(),
                                    });
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        notify();
                                    }
                                },
                            ) {
                                Ok(spool_path) => {
                                    if spool_only {
                                        Ok((
                                            format!("Created spool PDF: {}", spool_path.display()),
                                            Some(spool_path),
                                        ))
                                    } else {
                                        let _ = ev_tx.send(ExportEvent::Progress {
                                            fraction: 0.9,
                                            stage: "Sending to OS print spooler...".to_string(),
                                        });
                                        notify();
                                        print::dispatch_to_os_printer(&spool_path)
                                            .map(|msg| (msg, Some(spool_path)))
                                    }
                                }
                                Err(e) => Err(e),
                            }
                        }
                        ExportTarget::PrintWithOptions(opts) => {
                            let tx_prog = ev_tx.clone();
                            let mut last_pct = 0i32;
                            match print::create_print_spool_pdf_with_options_and_base_dir(
                                &blocks,
                                &req.title,
                                &opts,
                                base_dir,
                                |f| {
                                    let _ = tx_prog.send(ExportEvent::Progress {
                                        fraction: 0.2 + 0.65 * f,
                                        stage: "Rendering print spool PDF...".to_string(),
                                    });
                                    let pct = (f * 20.0) as i32;
                                    if pct != last_pct {
                                        last_pct = pct;
                                        notify();
                                    }
                                },
                            ) {
                                Ok(spool_path) => {
                                    let _ = ev_tx.send(ExportEvent::Progress {
                                        fraction: 0.9,
                                        stage: "Sending print job to printer...".to_string(),
                                    });
                                    notify();
                                    print::dispatch_to_os_printer_with_options(&spool_path, &opts)
                                        .map(|msg| (msg, Some(spool_path)))
                                }
                                Err(e) => Err(e),
                            }
                        }
                    };

                    match result {
                        Ok((message, output_path)) => {
                            let _ = ev_tx.send(ExportEvent::Completed {
                                message,
                                output_path,
                            });
                        }
                        Err(error) => {
                            let _ = ev_tx.send(ExportEvent::Failed { error });
                        }
                    }
                    notify();
                }
            })
            .expect("failed to spawn background exporter thread");

        Self {
            tx: req_tx,
            rx: ev_rx,
            busy: false,
            progress: 0.0,
            status_text: String::new(),
        }
    }

    /// Enqueues an export or print job using an O(1) cloned `Rope` snapshot.
    pub fn start_job(&mut self, rope: Rope, title: String, target: ExportTarget) {
        self.start_job_with_base_dir(rope, title, target, None);
    }

    /// Enqueues an export or print job with an optional base directory for relative image paths.
    pub fn start_job_with_base_dir(
        &mut self,
        rope: Rope,
        title: String,
        target: ExportTarget,
        base_dir: Option<PathBuf>,
    ) {
        self.busy = true;
        self.progress = 0.05;
        self.status_text = "Starting background job...".to_string();
        let _ = self.tx.send(ExportRequest {
            rope,
            title,
            target,
            base_dir,
        });
    }

    /// Drains pending worker events without blocking the UI thread.
    pub fn poll(&mut self) -> Vec<ExportEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            match &ev {
                ExportEvent::Started { label } => {
                    self.busy = true;
                    self.progress = 0.1;
                    self.status_text = label.clone();
                }
                ExportEvent::Progress { fraction, stage } => {
                    self.busy = true;
                    self.progress = *fraction;
                    self.status_text = stage.clone();
                }
                ExportEvent::Completed { message, .. } => {
                    self.busy = false;
                    self.progress = 1.0;
                    self.status_text = message.clone();
                }
                ExportEvent::Failed { error } => {
                    self.busy = false;
                    self.progress = 0.0;
                    self.status_text = format!("Export failed: {error}");
                }
            }
            events.push(ev);
        }
        events
    }
}
