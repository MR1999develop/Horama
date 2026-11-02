//! Renders the window to PNG files, to look at the interface without opening it:
//! `cargo test --test screenshot -- --ignored`, with HORAMA_SCREENSHOTS set to a folder.
//! Ignored by default: it needs a graphics adapter.

use egui_kittest::Harness;
use horama::egui_app::HoramaApp;
use std::path::Path;
use std::time::Duration;

#[test]
#[ignore]
fn window_in_both_themes() {
    let Some(out) = std::env::var_os("HORAMA_SCREENSHOTS") else {
        return;
    };
    let out = Path::new(&out).to_path_buf();
    let base = std::env::temp_dir().join(format!("horama-shot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (config, photos) = (base.join("config"), base.join("Photos"));
    std::fs::create_dir_all(config.join("Horama")).unwrap();
    std::fs::create_dir_all(photos.join("Trip")).unwrap();
    for (i, name) in ["sunset.jpg", "harbour.jpg", "Trip/hills.jpg", "Trip/lake.jpg"]
        .iter()
        .enumerate()
    {
        image::RgbImage::from_fn(600, 400, |x, y| {
            let k = i as u32 * 40;
            image::Rgb([((x / 3 + k) % 256) as u8, ((y / 2 + k) % 256) as u8, 160])
        })
        .save(photos.join(name))
        .unwrap();
    }
    for theme in ["Dark", "Light"] {
        std::fs::write(
            config.join("Horama").join("settings.json"),
            format!(
                r#"{{"last_folder":"","recent_folders":[],"custom_profiles":[],"language":"it","theme":"{theme}"}}"#
            ),
        )
        .unwrap();
        // SAFETY: the only test of this program; set before the interface starts.
        unsafe { std::env::set_var("HORAMA_CONFIG_DIR", &config) };
        let mut harness = Harness::builder()
            .with_size([1280.0, 800.0])
            .build_ui_state(
                |ui, app: &mut HoramaApp| app.ui(ui),
                HoramaApp::new(egui::Context::default()),
            );
        harness.state_mut().open_path(&photos.to_string_lossy());
        for _ in 0..300 {
            harness.step();
            std::thread::sleep(Duration::from_millis(10));
        }
        let image = harness.render().expect("rendered");
        image
            .save(out.join(format!("horama-{}.png", theme.to_lowercase())))
            .unwrap();
    }
    let _ = std::fs::remove_dir_all(&base);
}
