//! Renders the window to PNG files, to look at the interface without opening it:
//! `cargo test --test screenshot -- --ignored`, with HORAMA_SCREENSHOTS set to a folder.
//! Ignored by default: it needs a graphics adapter.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
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
    for (i, name) in [
        "sunset.jpg",
        "harbour.jpg",
        "Trip/hills.jpg",
        "Trip/lake.jpg",
    ]
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
    for (theme, language) in [("Dark", "it"), ("Light", "it"), ("Dark", "zh")] {
        std::fs::write(
            config.join("Horama").join("settings.json"),
            format!(
                r#"{{"last_folder":"","recent_folders":[],"custom_profiles":[],"language":"{language}","theme":"{theme}"}}"#
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
        if language == "zh" {
            // Chinese, with the crop bar open.
            image.save(out.join("horama-zh.png")).unwrap();
            harness.key_press(egui::Key::C);
            harness.run_steps(5);
            let image = harness.render().expect("rendered");
            image.save(out.join("horama-zh-crop.png")).unwrap();
            continue;
        }
        image
            .save(out.join(format!("horama-{}.png", theme.to_lowercase())))
            .unwrap();
        if theme == "Dark" {
            // The filters open, to see the whole folder panel.
            harness.get_by_label("Filtri").click();
            harness.run_steps(5);
            let image = harness.render().expect("rendered");
            image.save(out.join("horama-dark-filters.png")).unwrap();
            // The magnifier (middle button) and the color picker (right button).
            let center = harness
                .get_by_label_contains("Vista dell'immagine")
                .rect()
                .center();
            for (button, name) in [
                (egui::PointerButton::Middle, "loupe"),
                (egui::PointerButton::Secondary, "picker"),
            ] {
                harness.hover_at(center);
                harness.event(egui::Event::PointerButton {
                    pos: center,
                    button,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                });
                harness.run_steps(3);
                let image = harness.render().expect("rendered");
                image
                    .save(out.join(format!("horama-{}.png", name)))
                    .unwrap();
                harness.event(egui::Event::PointerButton {
                    pos: center,
                    button,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                });
                harness.key_press(egui::Key::Escape);
                harness.run_steps(3);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&base);
}
