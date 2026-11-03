//! `horama`: the Horama image viewer (egui interface).
//!
//! Only the window lives here (eframe); the interface itself is `horama::egui_app`.
//! Command line: `horama [--mcp] [file or folder]`, as used by "Open with"; `--mcp` (or the
//! `HORAMA_MCP` variable) starts the MCP server on stdin/stdout.
#![windows_subsystem = "windows"]

use horama::config::APP_NAME;
use horama::egui_app::HoramaApp;

struct Shell(HoramaApp);

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.0.on_exit();
        // Settings and ratings are written by a background thread: let it finish.
        if !horama::persist::flush(std::time::Duration::from_secs(10)) {
            log::warn!("Settings or ratings may not have been saved before exit");
        }
    }
}

fn main() -> eframe::Result {
    let _log = horama::logging::init(APP_NAME);
    horama::logging::log_panics();
    log::info!("{} {} started", APP_NAME, env!("CARGO_PKG_VERSION"));
    let launch = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-') && !a.trim().is_empty());
    // MCP server for AI clients.
    let mcp = std::env::args().any(|a| a == "--mcp" || a == "-mcp")
        || std::env::var_os("HORAMA_MCP").is_some();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([960.0, 520.0])
        .with_drag_and_drop(true);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| {
            let mut app = HoramaApp::new(cc.egui_ctx.clone());
            if mcp {
                app.enable_mcp();
            }
            match launch {
                Some(path) => app.open_path(&path),
                None => app.open_last_folder(),
            }
            Ok(Box::new(Shell(app)))
        }),
    )
}

/// The program icon (assets/icon.png, built into the program) for the window and the
/// taskbar.
fn window_icon() -> Option<egui::IconData> {
    let image = image::load_from_memory(include_bytes!("../assets/icon.png")).ok()?;
    let rgba = image.thumbnail(256, 256).to_rgba8();
    Some(egui::IconData {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}
