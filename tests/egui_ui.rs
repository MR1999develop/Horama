//! Tests of the egui interface through its accessibility tree (egui_kittest): the keys and
//! buttons a user presses, and what ends up on disk. They work on temporary files and keep
//! the settings in a temporary folder (`HORAMA_CONFIG_DIR`).

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use horama::egui_app::HoramaApp;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("horama-ui-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_jpeg(path: &Path) {
    image::RgbImage::from_fn(64, 48, |x, y| {
        image::Rgb([(x * 4) as u8, (y * 5) as u8, 90])
    })
    .save(path)
    .unwrap();
}

/// Steps the interface until `done` holds (workers answer in the background).
fn wait_for(
    harness: &mut Harness<'_, HoramaApp>,
    what: &str,
    done: impl Fn(&Harness<'_, HoramaApp>) -> bool,
) {
    for _ in 0..1000 {
        harness.step();
        if done(harness) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {}", what);
}

fn shows(harness: &Harness<'_, HoramaApp>, label: &str) -> bool {
    harness.query_by_label_contains(label).is_some()
}

#[test]
fn rating_recycle_bin_undo_rotation_and_rename() {
    let config = temp_dir("config");
    let settings = config.join("Horama").join("settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        r#"{"last_folder":"","recent_folders":[],"custom_profiles":[],"language":"en"}"#,
    )
    .unwrap();
    // Read by every settings access of this test process.
    std::env::set_var("HORAMA_CONFIG_DIR", &config);

    let photos = temp_dir("photos");
    for name in ["a.jpg", "b.jpg", "c.jpg"] {
        write_jpeg(&photos.join(name));
    }

    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .build_ui_state(
            |ui, app: &mut HoramaApp| app.ui(ui),
            HoramaApp::new(egui::Context::default()),
        );
    harness.state_mut().open_path(&photos.to_string_lossy());
    wait_for(&mut harness, "the folder", |h| shows(h, "3 images shown"));
    wait_for(&mut harness, "the first image", |h| shows(h, "a.jpg —"));

    // Rating: key 3, written to the ratings file of the temporary settings folder.
    harness.key_press(egui::Key::Num3);
    harness.step();
    assert!(horama::persist::flush(Duration::from_secs(10)));
    let ratings = std::fs::read_to_string(config.join("Horama").join("ratings.jsonl")).unwrap();
    assert!(
        ratings.contains("a.jpg") && ratings.contains("\"r\":3"),
        "{}",
        ratings
    );

    // Del takes the image out of the list at once; Undo puts it back, nothing is deleted.
    harness.key_press(egui::Key::Delete);
    wait_for(&mut harness, "the removal", |h| shows(h, "2 images shown"));
    assert!(shows(&harness, "moved to the Recycle Bin"));
    harness.get_by_label("Undo").click();
    wait_for(&mut harness, "the undo", |h| shows(h, "3 images shown"));
    assert!(photos.join("a.jpg").exists());

    // R rotates the file on disk without recompression: only the EXIF orientation changes.
    harness.key_press(egui::Key::R);
    wait_for(&mut harness, "the rotation", |_| {
        horama::image_loader::read_exif_orientation_from_file(&photos.join("a.jpg")) == 6
    });

    // F2 on the third image renames it.
    harness.key_press(egui::Key::ArrowRight);
    harness.step();
    harness.key_press(egui::Key::ArrowRight);
    wait_for(&mut harness, "the third image", |h| shows(h, "c.jpg —"));
    harness.key_press(egui::Key::F2);
    harness.run_steps(2);
    // The rename field has the focus, with the current name selected or not: replace it all.
    let focused = harness
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .filter(|n| n.is_focused())
        .count();
    assert_eq!(focused, 1, "the rename field has the focus");
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.step();
    harness.event(egui::Event::Text("renamed".into()));
    harness.step();
    harness.key_press(egui::Key::Enter);
    wait_for(&mut harness, "the rename", |_| {
        photos.join("renamed.jpg").exists()
    });
    assert!(!photos.join("c.jpg").exists());
    wait_for(&mut harness, "the renamed image in the list", |h| {
        shows(h, "renamed.jpg —")
    });

    // Files added or deleted by another program appear and disappear by themselves.
    write_jpeg(&photos.join("d.jpg"));
    wait_for(&mut harness, "the new file", |h| shows(h, "4 images shown"));
    std::fs::remove_file(photos.join("d.jpg")).unwrap();
    wait_for(&mut harness, "the deleted file", |h| {
        shows(h, "3 images shown")
    });

    // C crops the current image in memory (Enter applies); Ctrl+Z / Ctrl+Y undo and redo
    // it; Ctrl+S writes it next to the original, which stays as it was.
    harness.key_press(egui::Key::C);
    harness.step();
    harness.key_press(egui::Key::Enter);
    wait_for(&mut harness, "the crop", |h| shows(h, "58×43 px"));
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_for(&mut harness, "the undo", |h| shows(h, "64×48 px"));
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Y);
    wait_for(&mut harness, "the redo", |h| shows(h, "58×43 px"));
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::S);
    wait_for(&mut harness, "the saved crop", |_| {
        photos.join("renamed_1.jpg").exists()
    });
    assert_eq!(
        image::image_dimensions(photos.join("renamed.jpg")).unwrap(),
        (64, 48)
    );
    wait_for(&mut harness, "the saved file", |_| {
        image::image_dimensions(photos.join("renamed_1.jpg")).ok() == Some((58, 43))
    });

    // Shift+click in the filmstrip selects a range: the view compares the images side by
    // side; clicking one of them shows it alone.
    harness
        .get_by_label("Thumbnail of a.jpg")
        .click_modifiers(egui::Modifiers::SHIFT);
    wait_for(&mut harness, "the comparison grid", |h| {
        h.query_by_role_and_label(egui::accesskit::Role::Button, "b.jpg")
            .is_some()
    });
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "b.jpg")
        .click();
    wait_for(&mut harness, "b.jpg alone", |h| shows(h, "b.jpg —"));

    // F6 moves the keyboard focus between the panels: the folders (search box) come next
    // after the image view, and typing there no longer drives the viewer.
    harness.key_press(egui::Key::F6);
    harness.run_steps(2);
    let focused = harness
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .filter(|n| n.is_focused())
        .count();
    assert_eq!(focused, 1, "F6 focuses the search box");

    drop(harness);
    let _ = std::fs::remove_dir_all(&photos);
    let _ = std::fs::remove_dir_all(&config);
}
