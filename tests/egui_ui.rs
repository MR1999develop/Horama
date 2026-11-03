//! Tests of the egui interface through its accessibility tree (egui_kittest): the keys and
//! buttons a user presses, and what ends up on disk. They work on temporary files and keep
//! the settings in a temporary folder (`HORAMA_CONFIG_DIR`).

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use horama::egui_app::HoramaApp;
use horama::mcp_server::McpCommand;
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
    // SAFETY: set before the interface and its threads start; this is the only test of this
    // program, so no other thread reads the environment meanwhile.
    unsafe { std::env::set_var("HORAMA_CONFIG_DIR", &config) };

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

    // The stars of a thumbnail rate that image alone (the middle one: 3 stars).
    harness.get_by_label("Rating of b.jpg").click();
    harness.step();
    assert!(horama::persist::flush(Duration::from_secs(10)));
    let ratings = std::fs::read_to_string(config.join("Horama").join("ratings.jsonl")).unwrap();
    assert!(
        ratings
            .lines()
            .any(|l| l.contains("b.jpg") && l.contains("\"r\":3")),
        "{}",
        ratings
    );
    assert!(shows(&harness, "renamed.jpg —"), "the current image stays");

    // The zoom levels menu next to the percentage; the status bar shows the RAM cache and
    // the info panel the profiles in use.
    harness.get_by_label("Zoom Levels").click();
    harness.run_steps(2);
    harness.get_by_label("200%").click();
    wait_for(&mut harness, "the 200% zoom", |h| shows(h, "— 200%"));
    assert!(shows(&harness, "RAM cache:"));
    assert!(shows(&harness, "Profile in use"));

    // Files added, changed or deleted by another program appear, are marked and disappear
    // by themselves.
    write_jpeg(&photos.join("d.jpg"));
    wait_for(&mut harness, "the new file", |h| shows(h, "4 images shown"));
    wait_for(&mut harness, "the mark of the new file", |h| {
        shows(h, "Thumbnail of d.jpg (New)")
    });
    image::RgbImage::from_pixel(64, 48, image::Rgb([200, 10, 10]))
        .save(photos.join("b.jpg"))
        .unwrap();
    wait_for(&mut harness, "the mark of the changed file", |h| {
        shows(h, "Thumbnail of b.jpg (Changed)")
    });
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
    // Two more F6: filmstrip, then the image again, where the keys drive the viewer.
    harness.key_press(egui::Key::F6);
    harness.step();
    harness.key_press(egui::Key::F6);
    harness.run_steps(2);
    let focused = harness
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .filter(|n| n.is_focused())
        .count();
    assert_eq!(focused, 0, "the search box let the focus go");

    // MCP: the commands of an AI client act on the viewer and answer through their channel.
    let (mcp_tx, mcp_rx) = std::sync::mpsc::channel();
    harness.state_mut().attach_mcp(mcp_rx);
    let ask =
        |harness: &mut Harness<'_, HoramaApp>,
         cmd: fn(crossbeam_channel::Sender<Result<String, String>>) -> McpCommand| {
            let (tx, rx) = crossbeam_channel::bounded(1);
            mcp_tx.send(cmd(tx)).unwrap();
            harness.step();
            rx.recv_timeout(Duration::from_secs(5)).unwrap()
        };
    let selected = ask(&mut harness, |resp| McpCommand::SelectImage {
        filename: "A.JPG".into(),
        resp,
    });
    assert!(selected.unwrap().ends_with("a.jpg"));
    let rated = ask(&mut harness, |resp| McpCommand::SetRating {
        rating: 9,
        resp,
    });
    assert!(rated.is_ok());
    let (tx, rx) = crossbeam_channel::bounded(1);
    mcp_tx
        .send(McpCommand::GetCurrentImageInfo { resp: tx })
        .unwrap();
    harness.step();
    let info = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(info["rating"], 5, "{}", info);
    let (tx, rx) = crossbeam_channel::bounded(1);
    mcp_tx.send(McpCommand::ListImages { resp: tx }).unwrap();
    harness.step();
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .len(),
        4
    );

    // Several dropped images open by themselves: the rest of the folder stays out, also
    // when new files appear in it.
    let two = vec![
        photos.join("a.jpg").to_string_lossy().to_string(),
        photos.join("b.jpg").to_string_lossy().to_string(),
    ];
    harness.state_mut().open_files(two);
    wait_for(&mut harness, "the dropped images", |h| {
        shows(h, "2 selected files") && shows(h, "2 images shown")
    });
    write_jpeg(&photos.join("e.jpg"));
    harness.run_steps(40);
    std::thread::sleep(Duration::from_millis(400));
    harness.run_steps(5);
    assert!(shows(&harness, "2 images shown"));

    // A format Horama never rewrites (here GIF, as RAW) turns on screen only.
    let gif = photos.join("g.gif");
    write_jpeg(&gif);
    let before = std::fs::read(&gif).unwrap();
    harness.state_mut().open_path(&gif.to_string_lossy());
    wait_for(&mut harness, "the GIF", |h| {
        shows(h, "g.gif —") && shows(h, "64×48 px")
    });
    harness.key_press(egui::Key::R);
    wait_for(&mut harness, "the turned GIF", |h| shows(h, "48×64 px"));
    assert!(shows(&harness, "Rotation shown only"));
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_for(&mut harness, "the GIF back", |h| shows(h, "64×48 px"));
    assert_eq!(
        std::fs::read(&gif).unwrap(),
        before,
        "the file is never written"
    );

    drop(harness);
    let _ = std::fs::remove_dir_all(&photos);
    let _ = std::fs::remove_dir_all(&config);
}
