use eframe::egui;
use pdf_cleaner::{clean_pdf_guarded, is_pdf_path, CleanOptions, CleanReport};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

const ACCENT: egui::Color32 = egui::Color32::from_rgb(59, 130, 246);
const ACCENT_SOFT: egui::Color32 = egui::Color32::from_rgb(37, 99, 235);
const MUTED: egui::Color32 = egui::Color32::from_rgb(148, 163, 184);
const SUCCESS: egui::Color32 = egui::Color32::from_rgb(74, 222, 128);
const ERROR: egui::Color32 = egui::Color32::from_rgb(248, 113, 113);
const WARNING: egui::Color32 = egui::Color32::from_rgb(251, 191, 36);

pub fn run() {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([660.0, 640.0])
            .with_min_inner_size([540.0, 520.0])
            .with_icon(app_icon()),
        ..Default::default()
    };

    if let Err(err) = eframe::run_native(
        "PDF Empty Page Cleaner",
        native_options,
        Box::new(|cc| Ok(Box::new(CleanerApp::new(cc)))),
    ) {
        eprintln!("Could not start the application: {err}");
    }
}

struct CleanerApp {
    results: Vec<CleanReport>,
    status: String,
    active: bool,
    receiver: Option<Receiver<WorkerMsg>>,
    pending: Vec<PathBuf>,
    remove_graphics_only: bool,
    last_output_dir: Option<PathBuf>,
}

enum WorkerMsg {
    Started {
        index: usize,
        total: usize,
        name: String,
    },
    Finished(CleanReport),
    Done,
}

impl CleanerApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::from_rgb(13, 16, 22);
        visuals.faint_bg_color = egui::Color32::from_rgb(23, 27, 36);
        visuals.extreme_bg_color = egui::Color32::from_rgb(17, 20, 27);
        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(23, 27, 36);
        cc.egui_ctx.set_visuals(visuals);

        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        cc.egui_ctx.set_style(style);

        Self {
            results: Vec::new(),
            status: "Ready.".to_string(),
            active: false,
            receiver: None,
            pending: Vec::new(),
            remove_graphics_only: false,
            last_output_dir: None,
        }
    }

    fn browse(&mut self, ctx: &egui::Context) {
        let Some(files) = rfd::FileDialog::new()
            .add_filter("PDF files", &["pdf", "PDF"])
            .set_title("Choose PDF files to clean")
            .pick_files()
        else {
            return;
        };
        self.enqueue(files, ctx);
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|input| input.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }

        let mut pdfs = Vec::new();
        let mut ignored = 0usize;
        for file in dropped {
            match file.path {
                Some(path) if is_pdf_path(&path) => pdfs.push(path),
                _ => ignored += 1,
            }
        }

        if ignored > 0 {
            self.status = format!("Ignored {ignored} non-PDF item(s).");
        }
        if !pdfs.is_empty() {
            self.enqueue(pdfs, ctx);
        }
    }

    fn enqueue(&mut self, files: Vec<PathBuf>, ctx: &egui::Context) {
        let pdfs: Vec<PathBuf> = files.into_iter().filter(|path| is_pdf_path(path)).collect();
        if pdfs.is_empty() {
            return;
        }

        if self.active {
            let count = pdfs.len();
            self.pending.extend(pdfs);
            self.status = format!("Queued {count} more file(s).");
        } else {
            self.start_batch(pdfs, ctx);
        }
    }

    fn start_batch(&mut self, files: Vec<PathBuf>, ctx: &egui::Context) {
        self.active = true;
        let total = files.len();
        self.status = format!("Cleaning {total} file(s)...");

        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);

        let options = CleanOptions {
            remove_graphics_only_pages: self.remove_graphics_only,
        };
        let ctx = ctx.clone();

        thread::spawn(move || {
            for (index, file) in files.into_iter().enumerate() {
                let name = file
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file.display().to_string());
                let _ = sender.send(WorkerMsg::Started {
                    index: index + 1,
                    total,
                    name,
                });
                ctx.request_repaint();

                let report = clean_pdf_guarded(&file, options);
                let _ = sender.send(WorkerMsg::Finished(report));
                ctx.request_repaint();
            }
            let _ = sender.send(WorkerMsg::Done);
            ctx.request_repaint();
        });
    }

    fn poll_worker(&mut self, ctx: &egui::Context) {
        let Some(receiver) = self.receiver.take() else {
            return;
        };

        let mut done = false;
        loop {
            match receiver.try_recv() {
                Ok(WorkerMsg::Started { index, total, name }) => {
                    self.status = format!("Cleaning {index}/{total}: {name}");
                }
                Ok(WorkerMsg::Finished(report)) => {
                    if let Some(parent) = report.output.as_ref().and_then(|path| path.parent()) {
                        self.last_output_dir = Some(parent.to_path_buf());
                    }
                    self.results.push(report);
                }
                Ok(WorkerMsg::Done) => done = true,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }

        if done {
            self.active = false;
            if self.pending.is_empty() {
                self.status = "Ready.".to_string();
            } else {
                let files = std::mem::take(&mut self.pending);
                self.start_batch(files, ctx);
                return;
            }
        } else {
            self.receiver = Some(receiver);
        }

        if self.active {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }

    fn drop_zone(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let dragging = ctx.input(|input| !input.raw.hovered_files.is_empty());
        let size = egui::vec2(ui.available_width(), 152.0);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

        let hovered = response.hovered();
        let fill = if dragging {
            egui::Color32::from_rgb(24, 48, 82)
        } else if hovered {
            egui::Color32::from_rgb(21, 32, 50)
        } else {
            ui.visuals().faint_bg_color
        };
        let stroke = if dragging || hovered {
            egui::Stroke::new(2.0_f32, ACCENT)
        } else {
            egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(62, 72, 90))
        };

        let painter = ui.painter();
        painter.rect(rect, egui::Rounding::same(14.0), fill, stroke);

        let title = if dragging {
            "Drop PDF files to clean them"
        } else {
            "Drag & drop PDF files here"
        };
        let subtitle = if self.active {
            "New files will be queued"
        } else {
            "or click to choose files"
        };

        painter.text(
            rect.center() - egui::vec2(0.0, 16.0),
            egui::Align2::CENTER_CENTER,
            title,
            egui::FontId::proportional(21.0),
            egui::Color32::from_rgb(226, 232, 240),
        );
        painter.text(
            rect.center() + egui::vec2(0.0, 16.0),
            egui::Align2::CENTER_CENTER,
            subtitle,
            egui::FontId::proportional(13.0),
            if dragging { ACCENT } else { MUTED },
        );
        painter.text(
            rect.center() + egui::vec2(0.0, 46.0),
            egui::Align2::CENTER_CENTER,
            "Empty = no text and no images",
            egui::FontId::proportional(12.0),
            egui::Color32::from_rgb(100, 116, 139),
        );

        if response.clicked() {
            self.browse(ctx);
        }
    }

    fn result_card(&self, ui: &mut egui::Ui, report: &CleanReport) {
        let frame = egui::Frame::none()
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgb(42, 49, 62),
            ))
            .rounding(10.0)
            .inner_margin(egui::Margin::symmetric(14.0, 12.0));

        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let name = report
                    .source
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| report.source.display().to_string());
                ui.label(egui::RichText::new(name).strong().size(14.0));
                ui.label(
                    egui::RichText::new(format!("{} pages", report.total_pages))
                        .small()
                        .color(MUTED),
                );
            });

            if let Some(error) = &report.error {
                ui.label(
                    egui::RichText::new(format!("Could not clean: {error}"))
                        .color(ERROR)
                        .size(13.0),
                );
                return;
            }

            if report.removed_pages.is_empty() {
                ui.label(
                    egui::RichText::new("No empty pages found.")
                        .color(SUCCESS)
                        .size(13.0),
                );
            } else {
                ui.label(
                    egui::RichText::new(format!(
                        "Removed {} of {} empty {}.",
                        report.removed_pages.len(),
                        report.total_pages,
                        plural(report.removed_pages.len())
                    ))
                    .color(SUCCESS)
                    .size(13.0),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "Removed pages: {}",
                        pages_preview(&report.removed_pages)
                    ))
                    .small()
                    .color(MUTED),
                );
            }

            if let Some(output) = &report.output {
                let file_name = output
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| output.display().to_string());
                let replaced = if report.overwrote {
                    " (replaced existing file)"
                } else {
                    ""
                };
                let saved = ui
                    .label(
                        egui::RichText::new(format!("Saved as {file_name}{replaced}"))
                            .small()
                            .color(egui::Color32::from_rgb(125, 211, 252)),
                    )
                    .on_hover_text(output.display().to_string());
                let _ = saved;
            }

            for warning in report.warnings.iter().take(3) {
                ui.label(egui::RichText::new(warning).small().color(WARNING));
            }
        });
    }

    fn results_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let count = self.results.len();
            ui.label(egui::RichText::new("Results").size(15.0).strong());
            if count > 0 {
                ui.label(
                    egui::RichText::new(format!("({count})"))
                        .size(13.0)
                        .color(MUTED),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if count > 0 && ui.small_button("Clear").clicked() {
                    self.results.clear();
                }
            });
        });
        ui.separator();

        if self.results.is_empty() {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Cleaned files will show up here.")
                    .small()
                    .color(MUTED),
            );
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for report in &self.results {
                    self.result_card(ui, report);
                    ui.add_space(6.0);
                }
            });
    }
}

impl eframe::App for CleanerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_drops(ctx);
        self.poll_worker(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(22.0, 18.0)))
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("PDF Empty Page Cleaner")
                        .size(26.0)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new(
                        "Removes pages that have no text and no images. Nothing is uploaded.",
                    )
                    .size(13.0)
                    .color(MUTED),
                );
                ui.add_space(12.0);

                self.drop_zone(ui, ctx);

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let choose = egui::Button::new(
                        egui::RichText::new("Choose PDF files...")
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(ACCENT_SOFT)
                    .min_size(egui::vec2(170.0, 32.0));
                    if ui.add(choose).clicked() {
                        self.browse(ctx);
                    }

                    let can_open = self.last_output_dir.is_some();
                    let open =
                        egui::Button::new("Open output folder").min_size(egui::vec2(150.0, 32.0));
                    if ui.add_enabled(can_open, open).clicked() {
                        if let Some(dir) = &self.last_output_dir {
                            open_folder(dir);
                        }
                    }
                });

                ui.add_space(2.0);
                ui.checkbox(
                    &mut self.remove_graphics_only,
                    "Also remove pages that contain only vector graphics",
                )
                .on_hover_text(
                    "Pages with text or images are always kept. Enable this to also treat pages \
                     that only contain drawn shapes or lines as empty.",
                );

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if self.active {
                        ui.add(egui::Spinner::new().size(14.0));
                    }
                    let color = if self.active { ACCENT } else { MUTED };
                    ui.label(egui::RichText::new(&self.status).size(12.5).color(color));
                });

                ui.add_space(8.0);
                self.results_list(ui);
            });
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        "page"
    } else {
        "pages"
    }
}

fn pages_preview(pages: &[u32]) -> String {
    const LIMIT: usize = 14;
    let mut text = pages
        .iter()
        .take(LIMIT)
        .map(|page| page.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if pages.len() > LIMIT {
        text.push_str(&format!(", ... ({} total)", pages.len()));
    }
    text
}

fn open_folder(dir: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(dir).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(dir).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

fn app_icon() -> egui::IconData {
    const SIZE: usize = 64;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let background = [37u8, 99, 235, 255];
    let page = [248u8, 250, 252, 255];
    let line = [148u8, 163, 184, 255];

    for y in 0..SIZE {
        for x in 0..SIZE {
            let index = (y * SIZE + x) * 4;
            let mut color = [0u8, 0, 0, 0];

            let corner_radius = 12i32;
            let inside_rounded = !corner_outside(x as i32, y as i32, SIZE as i32, corner_radius);
            if inside_rounded {
                color = background;
            }

            if (18..46).contains(&x) && (10..54).contains(&y) {
                color = page;
            }

            let on_line = (24..40).contains(&x) && matches!(y, 22 | 30 | 38 | 46);
            if on_line && (18..46).contains(&x) && (10..54).contains(&y) {
                color = line;
            }

            if (38..46).contains(&x)
                && (10..18).contains(&y)
                && (x as i32 - 46) + (y as i32 - 10) > -8
            {
                color = background;
            }

            rgba[index..index + 4].copy_from_slice(&color);
        }
    }

    egui::IconData {
        rgba,
        width: SIZE as u32,
        height: SIZE as u32,
    }
}

fn corner_outside(x: i32, y: i32, size: i32, radius: i32) -> bool {
    let corners = [
        (radius, radius, x < radius && y < radius),
        (size - radius, radius, x >= size - radius && y < radius),
        (radius, size - radius, x < radius && y >= size - radius),
        (
            size - radius,
            size - radius,
            x >= size - radius && y >= size - radius,
        ),
    ];

    for (cx, cy, in_corner) in corners {
        if in_corner {
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy > radius * radius {
                return true;
            }
        }
    }
    false
}
