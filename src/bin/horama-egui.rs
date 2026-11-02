//! `horama-egui`: the new egui interface of Horama, under construction.
//!
//! Only the window lives here (eframe); the interface itself is `horama::egui_app`.
//! Command line: `horama-egui [file or folder]`, as used by "Open with".
#![windows_subsystem = "windows"]

use horama::config::APP_NAME;
use horama::egui_app::HoramaApp;

struct Shell(HoramaApp);

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Settings and ratings are written by a background thread: let it finish.
        if !horama::persist::flush(std::time::Duration::from_secs(10)) {
            log::warn!("Settings or ratings may not have been saved before exit");
        }
    }
}

fn main() -> eframe::Result {
    let _log = horama::logging::init(APP_NAME);
    log::info!(
        "{} {} (egui interface) started",
        APP_NAME,
        env!("CARGO_PKG_VERSION")
    );
    let launch = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-') && !a.trim().is_empty());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("{} (egui preview)", APP_NAME))
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([960.0, 520.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| {
            let mut app = HoramaApp::new(cc.egui_ctx.clone());
            match launch {
                Some(path) => app.open_path(&path),
                None => app.open_last_folder(),
            }
            Ok(Box::new(Shell(app)))
        }),
    )
}
