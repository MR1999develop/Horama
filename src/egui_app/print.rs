//! File > Print: the image as shown (crop and view rotation included) goes to the printing of
//! the system ([`crate::print`]). On Windows the print window of the system opens at once; on
//! Linux and macOS Horama's window chooses printer, copies and fit to page first.

use super::HoramaApp;
use super::i18n::{tr, trf};
use crate::browse;
use crate::print::{self, CupsJob, MAX_COPIES, PrintMethod, Printers};

/// Results of the print worker.
pub(super) enum PrintMsg {
    /// The printers of CUPS, for the open window.
    Printers(Result<Printers, String>),
    /// The image was handed to the printing of the system (or not). `Ok` carries what CUPS
    /// answered, empty on Windows.
    Done {
        name: String,
        result: Result<String, String>,
    },
}

/// The print window of Linux and macOS.
pub(super) struct PrintDialog {
    source: String,
    /// `None` while the printers are being looked for.
    printers: Option<Result<Printers, String>>,
    printer: String,
    copies: u32,
    fit_to_page: bool,
}

impl HoramaApp {
    /// File > Print (Ctrl+P) on the image shown.
    pub(super) fn start_print(&mut self) {
        let Some(source) = self.current_path() else {
            return;
        };
        match print::method() {
            PrintMethod::SystemDialog => self.print_image(source, None),
            PrintMethod::Cups => {
                self.print_dialog = Some(PrintDialog {
                    source,
                    printers: None,
                    printer: self.settings.last_printer.clone(),
                    copies: 1,
                    fit_to_page: true,
                });
                let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(super::Msg::Print(
                        PrintMsg::Printers(print::list_printers()),
                    ));
                    ctx.request_repaint();
                });
            }
        }
    }

    /// Writes the image as shown in sRGB to a print file, then hands it to the system: the
    /// print window of Windows (`job` = `None`) or CUPS. Runs on its own thread, since CUPS may
    /// take seconds and the disk queue must not wait for it.
    fn print_image(&mut self, source: String, job: Option<CupsJob>) {
        let edited = self.edited_pixels(&source);
        let turns = self.view_turns_of(&source);
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let (loader, color) = (self.preloader.loader(), self.color.clone());
        let profile = self.profile_of(&source);
        let name = browse::file_name_of(&source);
        self.status = trf("Preparing {} for printing…", &[&name]);
        std::thread::spawn(move || {
            let pixels = match edited {
                Some(d) => Ok(d),
                None => loader
                    .get_or_load(&source)
                    .map(|d| super::view_rotation::turned(d, turns)),
            };
            let result = pixels.and_then(|decoded| {
                let srgb = color.pixels_in_srgb(
                    &decoded.rgb_pixels,
                    &profile,
                    decoded.embedded_icc.as_deref(),
                );
                let icc = color.get_profile_bytes(crate::config::PROFILE_SRGB, None);
                let file = print::write_print_file(decoded.width, decoded.height, &srgb, &icc)?;
                match job {
                    None => print::open_system_print_dialog(&file).map(|()| String::new()),
                    Some(job) => print::print_with_cups(&job, &file),
                }
            });
            let _ = tx.send(super::Msg::Print(PrintMsg::Done { name, result }));
            ctx.request_repaint();
        });
    }

    pub(super) fn print_result(&mut self, msg: PrintMsg) {
        match msg {
            PrintMsg::Printers(found) => {
                if let Some(dialog) = &mut self.print_dialog {
                    if let Ok(p) = &found
                        && !p.names.contains(&dialog.printer)
                    {
                        dialog.printer = p
                            .default
                            .clone()
                            .or_else(|| p.names.first().cloned())
                            .unwrap_or_default();
                    }
                    dialog.printers = Some(found);
                }
            }
            PrintMsg::Done { name, result } => {
                self.status.clear();
                match result {
                    Ok(answer) if answer.is_empty() => {
                        log::info!("Print window opened for {}", name);
                    }
                    Ok(answer) => {
                        log::info!("Printed {}: {}", name, answer);
                        self.notify(trf("{} was sent to the printer.", &[&name]), false);
                    }
                    Err(e) => {
                        log::warn!("Printing {} failed: {}", name, e);
                        self.notify(trf("Cannot print {}: {}", &[&name, &e]), true);
                    }
                }
            }
        }
    }

    /// The print window of Linux and macOS.
    pub(super) fn print_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.print_dialog else {
            return;
        };
        let mut print = false;
        let mut cancel = false;
        super::widgets::dialog(ctx, tr("Print"))
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(browse::file_name_of(&dialog.source));
                match &dialog.printers {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(tr("Looking for printers…"));
                        });
                    }
                    Some(Err(e)) => {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            trf("Printers not available: {}", &[e]),
                        );
                    }
                    Some(Ok(p)) if p.names.is_empty() => {
                        ui.label(tr(
                            "No printer found. Add one in the settings of the system.",
                        ));
                    }
                    Some(Ok(p)) => {
                        egui::Grid::new("print-options")
                            .num_columns(2)
                            .show(ui, |ui| {
                                ui.label(tr("Printer"));
                                egui::ComboBox::from_id_salt("printer")
                                    .selected_text(dialog.printer.as_str())
                                    .show_ui(ui, |ui| {
                                        for name in &p.names {
                                            ui.selectable_value(
                                                &mut dialog.printer,
                                                name.clone(),
                                                name.as_str(),
                                            );
                                        }
                                    });
                                ui.end_row();
                                ui.label(tr("Copies"));
                                ui.add(
                                    egui::DragValue::new(&mut dialog.copies).range(1..=MAX_COPIES),
                                );
                                ui.end_row();
                            });
                        ui.checkbox(&mut dialog.fit_to_page, tr("Fit to page"));
                    }
                }
                let ready =
                    matches!(&dialog.printers, Some(Ok(p)) if p.names.contains(&dialog.printer));
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(ready, super::widgets::primary_button(ui, tr("Print")))
                        .clicked()
                    {
                        print = true;
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if cancel {
            self.print_dialog = None;
        } else if print {
            let job = CupsJob {
                printer: dialog.printer.clone(),
                copies: dialog.copies,
                fit_to_page: dialog.fit_to_page,
                title: browse::file_name_of(&dialog.source),
            };
            let source = dialog.source.clone();
            self.print_dialog = None;
            self.settings.last_printer = job.printer.clone();
            self.settings.save();
            self.print_image(source, Some(job));
        }
    }
}
